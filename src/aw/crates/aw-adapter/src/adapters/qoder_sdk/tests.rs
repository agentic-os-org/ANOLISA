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
printf '{"global":"%s/sdk.mjs"}\n' "$PWD"
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
        serde_json::from_slice(&fs::read(files.0.join("qoder-sdk.json")).unwrap()).unwrap()
    }
}

#[test]
fn resident_observation_fails_before_native_preparation() {
    for evidence in [json!({}), json!({"mode": "auto"})] {
        let fixture = Fixture::new();
        let error = QoderSdk
            .prepare(
                fixture.input(Some(evidence), false),
                &AtomicBool::new(false),
            )
            .err()
            .expect("resident evidence must fail admission");
        assert!(error
            .to_string()
            .contains("resident Qoder SDK does not support AgentSight evidence"));
        assert!(error.to_string().contains("evidence.mode to off"));
        assert!(!fixture.root.path().join("probed").exists());
        assert!(!fixture.root.path().join("launch").exists());
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
        assert!(!capabilities.events["tool.after"].contains(&"replace_result".into()));
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
