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
            aw_core::descriptor::verify(
                &descriptor,
                &bytes,
                "business",
                capabilities.capabilities,
                if availability == "required" {
                    aw_core::descriptor::Availability::Required
                } else {
                    aw_core::descriptor::Availability::NativeFallback
                },
            )
            .unwrap();
        }
    }
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
