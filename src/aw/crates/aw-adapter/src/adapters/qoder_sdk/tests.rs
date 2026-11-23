//! Observation admission precedes native SDK preparation and Query startup.

use super::*;
use crate::NativeOptions;
use aw_exec::CommandSpec;
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let parent =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/qoder-sdk-admission");
        fs::create_dir_all(&parent).unwrap();
        let root = tempfile::tempdir_in(parent).unwrap();
        fs::write(
            root.path().join("node"),
            r#"#!/bin/sh
: > probed
printf '{"global":{"entry":"%s/sdk.mjs","version":"1.0.57","worker_version":"1.1.79"}}\n' "$PWD"
"#,
        )
        .unwrap();
        fs::set_permissions(root.path().join("node"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.path().join("worker.mjs"), "// Admission fixture.\n").unwrap();
        fs::write(root.path().join("sdk.mjs"), "// Probe result.\n").unwrap();
        Self { root }
    }

    fn input(&self, evidence: Option<Value>, reloadable: bool) -> LaunchInput {
        let mut document = json!({"spec": {
            "agents": {"sdk": {"adapter": "qoder-sdk"}},
            "execution": {"default_event_budget_ms": 1000}, "events": {}
        }});
        if let Some(evidence) = evidence {
            document["spec"]["agents"]["sdk"]["evidence"] = evidence;
        }
        LaunchInput {
            document,
            target: "sdk".into(),
            options: NativeOptions {
                reloadable,
                ..Default::default()
            },
            command: CommandSpec {
                program: self.root.path().join("node"),
                args: vec![self.root.path().join("worker.mjs").into_os_string()],
                cwd: self.root.path().into(),
                environment: BTreeMap::new(),
            },
        }
    }

    fn prepare_and_configure(&self, evidence: Option<Value>, reloadable: bool) -> Value {
        let mut prepared = QoderSdk
            .prepare(self.input(evidence, reloadable), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(prepared.capabilities().version, "1.0.57");
        let launch = self.root.path().join("launch");
        fs::create_dir(&launch).unwrap();
        let files = crate::test_support::Artifacts(launch);
        let plan = prepared
            .configure(&LaunchContext {
                files: &files,
                binding_path: &self.root.path().join("binding"),
                executable: &self.root.path().join("aw"),
                steps: &[],
            })
            .unwrap();
        assert!(self.root.path().join("probed").is_file());
        assert!(!plan.readiness.unwrap().path.exists());
        let config: Value =
            serde_json::from_slice(&fs::read(files.0.join("qoder-sdk.json")).unwrap()).unwrap();
        assert_eq!(config["sdk_metadata"]["global"]["version"], "1.0.57");
        assert_eq!(config["sdk_metadata"]["global"]["worker_version"], "1.1.79");
        config
    }
}

#[test]
fn resident_observation_preserves_the_resident_factory() {
    for evidence in [json!({}), json!({"mode": "auto"})] {
        let fixture = Fixture::new();
        let config = fixture.prepare_and_configure(Some(evidence), false);
        assert_eq!(config["resident"], true);
        assert_eq!(config["reloadable"], false);
        assert_eq!(config["resident_policy_reload"], true);
    }
}

#[test]
fn resident_without_observation_prepares_normally() {
    for evidence in [None, Some(json!({"mode": "off"}))] {
        let fixture = Fixture::new();
        let config = fixture.prepare_and_configure(evidence, false);
        assert_eq!(config["resident"], true);
        assert_eq!(config["reloadable"], false);
    }
}

#[test]
fn reloadable_observation_retains_its_existing_admission() {
    let fixture = Fixture::new();
    let config = fixture.prepare_and_configure(Some(json!({"mode": "auto"})), true);
    assert_eq!(config["resident"], false);
    assert_eq!(config["reloadable"], true);
}

#[test]
fn only_resident_queries_admit_post_withholding() {
    for reloadable in [false, true] {
        let fixture = Fixture::new();
        let prepared = QoderSdk
            .prepare(fixture.input(None, reloadable), &AtomicBool::new(false))
            .unwrap();
        let capabilities = prepared.capabilities();
        assert_eq!(
            capabilities.failure_actions.get("tool.after").cloned(),
            if reloadable {
                None
            } else {
                Some(vec!["withhold_result".into()])
            }
        );
        assert_eq!(
            capabilities.events["tool.after"].contains(&"replace_result".into()),
            !reloadable
        );
    }
}

#[test]
fn command_consumer_grants_are_checked_before_the_sdk_probe() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let path = fixture.root.path().join("consumer.sh");
    fs::write(&path, "#!/bin/sh\n").unwrap();
    let digest = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
    let mut input = fixture.input(None, false);
    input.document["spec"]["agents"]["sdk"]["qoder_sdk"] = json!({
        "command_wrappers":[{"path":path,"sha256":digest}]
    });
    QoderSdk.prepare(input, &AtomicBool::new(false)).unwrap();
    fs::remove_file(fixture.root.path().join("probed")).unwrap();
    fs::write(&path, "changed").unwrap();
    let mut input = fixture.input(None, false);
    input.document["spec"]["agents"]["sdk"]["qoder_sdk"] = json!({
        "command_wrappers":[{"path":path,"sha256":digest}]
    });
    assert!(QoderSdk.prepare(input, &AtomicBool::new(false)).is_err());
    assert!(!fixture.root.path().join("probed").exists());
}

#[test]
fn declared_sdk_sites_must_all_be_resolved_by_the_probe() {
    let fixture = Fixture::new();
    let mut input = fixture.input(None, false);
    input.document["spec"]["agents"]["sdk"]["qoder_sdk"] = json!({"sites":["global","cn"]});
    assert!(QoderSdk.prepare(input, &AtomicBool::new(false)).is_err());
    assert!(fixture.root.path().join("probed").exists());
}

#[test]
fn reloadable_execution_grants_fail_before_the_sdk_probe() {
    for grant in [
        json!({"declarative_plugins":true}),
        json!({"command_wrappers":[{"path":"/absent/wrapper","sha256":"0".repeat(64)}]}),
    ] {
        let fixture = Fixture::new();
        let mut input = fixture.input(None, true);
        input.document["spec"]["agents"]["sdk"]["qoder_sdk"] = grant;
        let error = QoderSdk
            .prepare(input, &AtomicBool::new(false))
            .err()
            .unwrap();
        assert!(error.to_string().contains("ordinary resident mode"));
        assert!(!fixture.root.path().join("probed").exists());
        assert!(!fixture.root.path().join("launch").exists());
    }
}

fn prepare_final(
    fixture: &Fixture,
    steps: Value,
    reloadable: bool,
) -> (aw_config::Configuration, Box<dyn PreparedLaunch>) {
    let mut profile: Value = serde_json::from_str::<Vec<Value>>(include_str!(
        "../../../native/qoder-sdk/final-profiles.json"
    ))
    .unwrap()
    .remove(0);
    profile["runtime_target"] = profile["target"].clone();
    fs::write(
        fixture.root.path().join("final-profile.json"),
        profile.to_string(),
    )
    .unwrap();
    fs::write(
        fixture.root.path().join("node"),
        r#"#!/bin/sh
if [ "$#" = 5 ]; then
    : > probed-final
    cat final-profile.json
else
    printf '{"global":{"entry":"%s/sdk.mjs","version":"1.0.57","worker_version":"1.1.81"}}\n' "$PWD"
fi
"#,
    )
    .unwrap();
    let mut input = fixture.input(None, reloadable);
    input.document = crate::test_support::final_gate_document("qoder-sdk", steps);
    let config = aw_config::Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&input.document).unwrap())
        .unwrap();
    let prepared = crate::select("qoder-sdk")
        .unwrap()
        .prepare(input, &AtomicBool::new(false))
        .unwrap();
    (config, prepared)
}

#[test]
fn off_final_steps_pass_launch_preflight_without_the_final_probe() {
    let fixture = Fixture::new();
    let (config, prepared) = prepare_final(
        &fixture,
        json!([crate::test_support::final_gate_step("off", "block")]),
        true,
    );
    let capabilities = prepared.capabilities();
    assert_eq!(capabilities.entrypoint, "sdk-worker");
    assert!(!capabilities.events.contains_key("security.violation"));
    assert!(!fixture.root.path().join("probed-final").exists());
    let admitted = aw_core::admission::preflight(&config, "sdk", &capabilities).unwrap();
    assert!(admitted.is_empty());
    prepared.validate_steps(&admitted).unwrap();

    let (config, prepared) = prepare_final(&fixture, json!([]), false);
    assert_eq!(prepared.capabilities().entrypoint, "sdk-worker");
    let rejected =
        aw_core::admission::preflight(&config, "sdk", &prepared.capabilities()).unwrap_err();
    assert!(rejected
        .to_string()
        .contains("evidence lacks an enabled event"));
    assert!(!fixture.root.path().join("probed-final").exists());
}

#[test]
fn shadow_final_gate_passes_launch_validation_and_enforce_remains_fail_closed() {
    for (mode, on_error, expected_action, success) in [
        ("shadow", "block", "report", true),
        ("enforce", "block", "block", true),
        ("enforce", "report", "report", false),
    ] {
        let fixture = Fixture::new();
        let (config, mut prepared) = prepare_final(
            &fixture,
            json!([crate::test_support::final_gate_step(mode, on_error)]),
            false,
        );
        let capabilities = prepared.capabilities();
        assert_eq!(capabilities.entrypoint, "foreground-final-worker");
        assert!(fixture.root.path().join("probed-final").exists());
        let steps = aw_core::admission::preflight(&config, "sdk", &capabilities).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].on_error, expected_action);
        if success {
            prepared.validate_steps(&steps).unwrap();
            let launch = fixture.root.path().join("launch");
            fs::create_dir(&launch).unwrap();
            let files = crate::test_support::Artifacts(launch);
            prepared
                .configure(&LaunchContext {
                    files: &files,
                    binding_path: &fixture.root.path().join("binding"),
                    executable: &fixture.root.path().join("aw"),
                    steps: &steps,
                })
                .unwrap();
            let private: Value =
                serde_json::from_slice(&fs::read(files.0.join("qoder-sdk.json")).unwrap()).unwrap();
            assert_eq!(private["final_enforcing"], mode == "enforce");
        } else {
            assert!(prepared
                .validate_steps(&steps)
                .unwrap_err()
                .to_string()
                .contains("requires structured fail-closed final steps"));
        }
    }
}

#[test]
fn native_shadow_is_rejected_and_the_final_adapter_checks_native_steps() {
    let validator = aw_config::Validator::new().unwrap();
    let fixture = Fixture::new();
    let (config, prepared) = prepare_final(
        &fixture,
        json!([crate::test_support::final_gate_step("shadow", "block")]),
        false,
    );
    let native = |mode: &str| {
        json!({
            "id":"native","provider":"native","mode":mode,"native":{},"on_error":"report"
        })
    };
    let mut document = config.as_value().clone();
    let mut provider = document["spec"]["providers"]["policy"].clone();
    provider["protocol"] = json!("native-hook/v1alpha1");
    document["spec"]["providers"]["native"] = provider;
    let mut shadow = document.clone();
    shadow["spec"]["events"]["security.violation"]["steps"] = json!([native("shadow")]);
    let Err(error) = validator.parse(&serde_json::to_vec(&shadow).unwrap()) else {
        panic!("native shadow final step was accepted");
    };
    assert!(error
        .to_string()
        .contains("shadow and decisions require a structured Provider"));

    document["spec"]["events"]["tool.before"] = json!({"enabled":true,"steps":[native("enforce")]});
    let config = validator
        .parse(&serde_json::to_vec(&document).unwrap())
        .unwrap();
    let mut steps =
        aw_core::admission::preflight(&config, "sdk", &prepared.capabilities()).unwrap();
    assert!(prepared
        .validate_steps(&steps)
        .unwrap_err()
        .to_string()
        .contains("structured Provider steps"));
    // Defend the final Adapter boundary even when passed an unsupported native step.
    steps
        .iter_mut()
        .find(|step| step.event == "tool.before")
        .unwrap()
        .event = "security.violation".into();
    assert!(prepared
        .validate_steps(&steps)
        .unwrap_err()
        .to_string()
        .contains("fail-closed final steps"));
}
