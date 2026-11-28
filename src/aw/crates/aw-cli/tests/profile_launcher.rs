//! Compiled builtin profiles exercise the same CLI and SDK startup as consumers.

#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

#[test]
fn builtin_profile_launch_and_cooperative_control() {
    let fixture = support::Fixture::new();
    let mut command = std::process::Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/launcher/instrumentation.py"
        ))
        .arg(env!("CARGO_BIN_EXE_aw"));
    let output = support::Process::spawn(&fixture, command)
        .finish_with_timeout(std::time::Duration::from_secs(120));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn builtin_projection_drives_descriptor_and_shadow_preflight() {
    use aw_core::{admission, resolver};
    use serde_json::json;

    let temp = tempfile::tempdir().unwrap();
    let validator = aw_config::Validator::new().unwrap();
    let mut value = validator
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    value["spec"]["daemon"]["state_dir"] = json!(temp.path());
    value["spec"]["events"] = json!({"tool.before":{"enabled":true,"required":true,"steps":[]}});
    value["spec"]["agents"] = json!({"app":{
        "adapter":"sdk", "argv":["python3","app.py"], "sdk":{"profiles":["fixture.python","fixture.second"]}
    }});
    let bytes = serde_json::to_vec(&value).unwrap();
    let config = validator.parse(&bytes).unwrap();
    let profiles =
        aw_config::instrumentation::profiles("app", &value["spec"]["agents"]["app"]).unwrap();
    let capabilities = aw_adapter::deployment::instrumentation_capabilities(&profiles).unwrap();
    assert_eq!(capabilities.events["tool.before"], ["observe"]);
    let descriptor = aw_core::descriptor::compile(
        &bytes,
        "app",
        capabilities.clone(),
        aw_core::descriptor::Availability::Required,
    )
    .unwrap();
    let binding = descriptor.instrumentation_profiles().unwrap();
    assert_eq!(binding.profiles.len(), 2);
    assert_eq!(binding.events["tool.before"], ["observe"]);
    let explicit = resolver::resolve_configuration(&config, "app", &[]).unwrap();
    let (shadow, steps) =
        resolver::resolve_shadow_preflight(&config, "app", &capabilities).unwrap();
    assert_eq!(explicit, shadow);
    assert!(steps.is_empty());
    let mut exaggerated = capabilities;
    exaggerated
        .events
        .get_mut("tool.before")
        .unwrap()
        .push("block".into());
    assert!(admission::preflight(&config, "app", &exaggerated).is_err());
    assert!(resolver::resolve_shadow_preflight(&config, "app", &exaggerated).is_err());
}

#[test]
fn generic_application_descriptor_retains_the_public_sdk_identity() {
    use serde_json::json;
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let config_path = root.path().join("aw.json");
    let mut value = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    value["spec"]["agents"] = json!({"app": {"adapter": "sdk", "argv": ["python3"]}});
    fs::write(&config_path, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aw"))
        .env("XDG_RUNTIME_DIR", root.path())
        .args(["config", "descriptor", "--config"])
        .arg(config_path)
        .args(["--agent", "app", "--profile", "application-hooks"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let descriptor = aw_contracts::deployment::DeploymentDescriptor::parse(&output.stdout).unwrap();
    assert_eq!(descriptor.profile().adapter, "sdk");
    assert_eq!(descriptor.profile().version, "aw-hook-source/v1");
    assert_eq!(descriptor.profile().entrypoint, "application");
    assert!(descriptor.instrumentation_profiles().is_none());
    assert!(!root.path().join("aw").exists());
}
