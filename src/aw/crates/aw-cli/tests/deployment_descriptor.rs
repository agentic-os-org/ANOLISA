//! Deployment compilation rejects unsupported wiring before any runtime startup.

use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

fn configuration(root: &std::path::Path, adapter: &str) -> Value {
    let mut document = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    document["spec"]["agents"] = json!({"business":{"adapter":adapter,"argv":["/absent/agent"]}});
    document["spec"]["daemon"]["state_dir"] = json!(root.join("state"));
    document["spec"]["daemon"]["endpoint"] = json!("auto");
    document["spec"]["providers"] = json!({});
    document["spec"]["events"] = json!({});
    document
}

fn invoke(root: &std::path::Path, document: &Value, profile: &str) -> std::process::Output {
    let config = root.join("aw.json");
    fs::write(&config, serde_json::to_vec(document).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    Command::new(env!("CARGO_BIN_EXE_aw"))
        .args(["config", "descriptor", "--config"])
        .arg(config)
        .args(["--agent", "business", "--profile", profile])
        .output()
        .unwrap()
}

#[test]
fn descriptor_cli_uses_adapter_profile_choices() {
    let help = Command::new(env!("CARGO_BIN_EXE_aw"))
        .args(["config", "descriptor", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for name in aw_adapter::deployment::profile_names() {
        assert!(help.contains(name), "profile missing from CLI help: {name}");
        let profile = aw_adapter::deployment::profile(name, false).unwrap();
        let document = configuration(directory.path(), &profile.capabilities.adapter);
        let output = invoke(directory.path(), &document, name);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let rejected = Command::new(env!("CARGO_BIN_EXE_aw"))
        .args([
            "config",
            "descriptor",
            "--config",
            "/absent/config",
            "--agent",
            "business",
            "--profile",
            "unknown-profile",
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("invalid value"));
}

#[test]
fn descriptor_binds_exact_configuration_and_selects_native_waiting() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for (adapter, profile, availability) in [
        ("qoder-sdk", "qoder-public-hooks", "native_fallback"),
        ("qwenpaw", "qwenpaw-installed-native-hooks", "required"),
    ] {
        let mut document = configuration(directory.path(), adapter);
        for input in [false, true] {
            document["spec"]["events"]["input.submit"] =
                json!({"enabled":input,"required":false,"steps":[]});
            let output = invoke(directory.path(), &document, profile);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["target"], "business");
            assert_eq!(value["profile"]["adapter"], adapter);
            assert_eq!(value["wait_for_input_submit"], input);
            assert_eq!(value["availability"], availability);
            assert_eq!(
                value["endpoint"],
                json!(directory.path().join("state/aw.sock"))
            );
            let bytes = fs::read(directory.path().join("aw.json")).unwrap();
            let capabilities = aw_adapter::deployment::profile(profile, input).unwrap();
            let descriptor =
                aw_core::descriptor::DeploymentDescriptor::parse(&output.stdout).unwrap();
            aw_core::descriptor::verify_with_options(
                &descriptor,
                &bytes,
                "business",
                capabilities.capabilities,
                if availability == "required" {
                    aw_core::descriptor::Availability::Required
                } else {
                    aw_core::descriptor::Availability::NativeFallback
                },
                aw_core::descriptor::CompileOptions {
                    session_start_hint: capabilities.session_start_hint,
                },
            )
            .unwrap();
        }
    }
    assert!(!directory.path().join("state").exists());
}

#[test]
fn descriptor_availability_uses_target_requirements_and_profile_minimum() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for (adapter, profile, always_required) in [
        ("qoder-sdk", "qoder-public-hooks", false),
        ("qwenpaw", "qwenpaw-installed-native-hooks", true),
    ] {
        for (selector, applicable) in [
            ("native:pi:Shell".to_owned(), false),
            (format!("native:{adapter}:Shell"), true),
            ("*".to_owned(), true),
        ] {
            let mut document = configuration(directory.path(), adapter);
            document["spec"]["agents"]["pi"] = json!({"adapter":"pi","argv":["/absent/agent"]});
            document["spec"]["events"]["tool.before"] = json!({
                "enabled":true,"required":true,"steps":[],"match":{"tools":[selector]}
            });
            let output = invoke(directory.path(), &document, profile);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                value["availability"],
                if always_required || applicable {
                    "required"
                } else {
                    "native_fallback"
                }
            );
        }
    }
    assert!(!directory.path().join("state").exists());
}

#[test]
fn qoder_public_descriptor_admits_replacement_with_report_in_both_availability_modes() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mut document = configuration(directory.path(), "qoder-sdk");
    document["spec"]["providers"]["policy"] = json!({
        "protocol":"aw-provider/v1alpha1","config":{},
        "transport":{"type":"stdio","location":"agent","argv":["/absent/provider"]},
        "timeout_ms":1000,"max_output_bytes":4096
    });
    document["spec"]["events"]["tool.after"] = json!({
        "enabled":true,"required":false,"steps":[{
            "id":"replace","provider":"policy","operation":"scan",
            "effects":["observe","replace_result"],"on_error":"report"
        }]
    });
    for (required, availability) in [(false, "native_fallback"), (true, "required")] {
        document["spec"]["events"]["tool.after"]["required"] = json!(required);
        let output = invoke(directory.path(), &document, "qoder-public-hooks");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["availability"], availability);
    }
    document["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!("withhold_result");
    let output = invoke(directory.path(), &document, "qoder-public-hooks");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failure action"));
    assert!(!directory.path().join("state").exists());
}

#[test]
fn descriptor_rejects_profile_mismatch_and_unsupported_empty_hooks() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mut document = configuration(directory.path(), "qwenpaw");
    let output = invoke(directory.path(), &document, "qoder-public-hooks");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    document["spec"]["events"]["model.before_request"] =
        json!({"enabled":true,"required":false,"steps":[]});
    let output = invoke(
        directory.path(),
        &document,
        "qwenpaw-installed-native-hooks",
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!directory.path().join("state").exists());
}

#[test]
fn descriptor_retains_configuration_file_trust_checks() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = directory.path();
    let document = configuration(root, "qoder-sdk");
    let path = root.join("aw.json");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(["config", "descriptor", "--config"])
            .arg(&path)
            .args(["--agent", "business", "--profile", "qoder-public-hooks"])
            .output()
            .unwrap()
    };
    let rejected = || {
        let output = run();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!root.join("state").exists());
    };
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    for mode in [0o666, 0o620, 0o4600] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        rejected();
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = root.join("alias.json");
    fs::hard_link(&path, &alias).unwrap();
    rejected();
    fs::remove_file(&alias).unwrap();
    fs::rename(&path, &alias).unwrap();
    symlink(&alias, &path).unwrap();
    rejected();
    fs::remove_file(&path).unwrap();
    fs::rename(&alias, &path).unwrap();
    fs::set_permissions(root, fs::Permissions::from_mode(0o777)).unwrap();
    rejected();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(run().status.success());
}

#[test]
fn show_selects_only_one_matching_target_without_disclosing_configuration() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().join("aw.json");
    let mut document = configuration(directory.path(), "qoder-sdk");
    let show = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(["config", "show", "--config"])
            .arg(&path)
            .args(args)
            .output()
            .unwrap()
    };
    document["spec"]["agents"]["second"] = document["spec"]["agents"]["business"].clone();
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    for args in [
        vec!["--sdk-target", "qoder-sdk"],
        vec!["--sdk-target", "qwenpaw/business"],
        vec!["--sdk-target", "qoder-sdk/missing"],
    ] {
        let output = show(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            json!({"target":null})
        );
    }
    let output = show(&["--sdk-target", "qoder-sdk/second"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({
            "target": "second", "daemon": {"state_dir": directory.path().join("state"), "endpoint": "auto"},
            "input_enabled": false, "target_requires_service": false
        })
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&show(&[]).stdout).unwrap(),
        document
    );
    assert!(!show(&["--agent", "second"]).status.success());
    assert!(
        !show(&["--sdk-target", "qoder-sdk", "--event", "tool.before"])
            .status
            .success()
    );
    assert!(!show(&["--sdk-target", "qoder-sdk", "--id", "step"])
        .status
        .success());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(!show(&["--sdk-target", "qoder-sdk"]).status.success());
}

#[test]
fn shared_configuration_scopes_descriptor_and_sdk_hints_to_the_selected_target() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mut document = configuration(directory.path(), "qoder-sdk");
    document["spec"]["agents"] = json!({
        "qoder": {"adapter": "qoder-sdk", "argv": ["/absent/agent"]},
        "qwenpaw": {"adapter": "qwenpaw", "argv": ["/absent/agent"]},
        "claude": {"adapter": "claude-sdk", "argv": ["/absent/agent"],
            "claude_sdk":{"allow_plaintext_http_hosts":["model-proxy.cluster.local:8080"]}}
    });
    for event in ["session.start", "input.submit"] {
        document["spec"]["events"][event] = json!({
            "enabled": true, "steps": [], "match": {"agents": ["qoder"]}
        });
    }
    let config = directory.path().join("aw.yaml");
    fs::write(&config, serde_yaml_ng::to_string(&document).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    for (target, profile, adapter, selected) in [
        ("qoder", "qoder-public-hooks", "qoder-sdk", true),
        ("claude", "claude-resident-hooks", "claude-sdk", false),
        (
            "qwenpaw",
            "qwenpaw-installed-native-hooks",
            "qwenpaw",
            false,
        ),
    ] {
        let run = |command: &str, args: &[&str]| -> Value {
            let output = Command::new(env!("CARGO_BIN_EXE_aw"))
                .args(["config", command, "--config"])
                .arg(&config)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{target}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice(&output.stdout).unwrap()
        };
        let descriptor = run("descriptor", &["--agent", target, "--profile", profile]);
        let shown = run("show", &["--sdk-target", adapter]);
        for view in [&descriptor, &shown] {
            assert_eq!(view["target"], target);
            assert_eq!(
                view.get("allow_plaintext_http_hosts"),
                (target == "claude")
                    .then_some(json!(["model-proxy.cluster.local:8080"]))
                    .as_ref()
            );
            assert_eq!(
                view.get("observe_session_start"),
                selected.then_some(json!(true)).as_ref()
            );
        }
        assert_eq!(descriptor["wait_for_input_submit"], selected);
        assert_eq!(shown["input_enabled"], selected);
    }
}

#[test]
fn session_start_hint_is_scoped_to_qoder_public_hooks() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let base_fields = [
        "availability",
        "configuration_sha256",
        "endpoint",
        "profile",
        "target",
        "version",
        "wait_for_input_submit",
    ];
    for (adapter, profile, expects_hint) in [
        ("qoder-sdk", "qoder-public-hooks", true),
        ("claude-sdk", "claude-resident-hooks", false),
        ("pi", "pi-worker-hooks", false),
    ] {
        let mut document = configuration(directory.path(), adapter);
        document["spec"]["events"]["session.start"] =
            json!({"enabled": true, "required": false, "steps": []});
        let output = invoke(directory.path(), &document, profile);
        assert!(
            output.status.success(),
            "{profile}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        if expects_hint {
            assert_eq!(value["observe_session_start"], json!(true));
        } else {
            assert!(
                value.get("observe_session_start").is_none(),
                "{profile} unexpectedly emitted observe_session_start"
            );
            assert_eq!(keys, base_fields);
        }
        let bytes = fs::read(directory.path().join("aw.json")).unwrap();
        let capabilities = aw_adapter::deployment::profile(profile, false).unwrap();
        let descriptor = aw_core::descriptor::DeploymentDescriptor::parse(&output.stdout).unwrap();
        aw_core::descriptor::verify_with_options(
            &descriptor,
            &bytes,
            "business",
            capabilities.capabilities,
            if capabilities.requires_service {
                aw_core::descriptor::Availability::Required
            } else {
                aw_core::descriptor::Availability::NativeFallback
            },
            aw_core::descriptor::CompileOptions {
                session_start_hint: capabilities.session_start_hint,
            },
        )
        .unwrap();

        let show = Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(["config", "show", "--config"])
            .arg(directory.path().join("aw.json"))
            .args(["--sdk-target", adapter])
            .output()
            .unwrap();
        assert!(
            show.status.success(),
            "{adapter}: {}",
            String::from_utf8_lossy(&show.stderr)
        );
        let shown: Value = serde_json::from_slice(&show.stdout).unwrap();
        assert_eq!(
            shown.get("observe_session_start") == Some(&json!(true)),
            expects_hint
        );
    }
    assert!(!directory.path().join("state").exists());
}
