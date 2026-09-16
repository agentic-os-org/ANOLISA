//! Subprocess coverage for `forget --dry-run` when adapter receipts exist.
//!
//! Execute already refuses a component that still has enabled adapters.
//! Dry-run must preview that same refusal (error envelope, exit 2) instead
//! of a successful "would forget" payload. An unrelated receipt must not
//! block this component.
//!
//! Subprocesses stay on `--dry-run` with `--install-mode system` and a
//! temporary `--prefix`. A real system-mode forget is a ModeScopedMutation
//! and needs root; the privilege gate would hide the adapter envelope on
//! non-root runners. In-process `forget_refuses_with_enabled_adapter_claim`
//! covers the execute path.

use std::path::PathBuf;
use std::process::Output;

use anolisa_core::adapter::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimStatus, DRIVER_SCHEMA_VERSION, DriverPayload,
    OpenClawClaim,
};
use anolisa_core::{
    InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind, ObjectStatus,
    Ownership, RpmMetadata, SubscriptionScope,
};
use anolisa_platform::fs_layout::FsLayout;
use serde_json::Value;

mod common;

const TARGET: &str = "copilot-shell";
const OTHER: &str = "tokenless";
const FRAMEWORK: &str = "openclaw";

struct ForgetFixture {
    _tmp: tempfile::TempDir,
    prefix: PathBuf,
}

impl ForgetFixture {
    fn with_claim_on(component: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prefix = tmp.path().join("system");
        seed_index(&prefix, &[TARGET, OTHER]);
        plant_state(&prefix, component);
        Self { _tmp: tmp, prefix }
    }

    fn run_dry_run(&self) -> Output {
        let prefix = self.prefix.to_string_lossy().into_owned();
        common::run(&[
            "--json",
            "--no-color",
            "--dry-run",
            "--install-mode",
            "system",
            "--prefix",
            prefix.as_str(),
            "forget",
            TARGET,
        ])
    }
}

fn seed_index(prefix: &std::path::Path, components: &[&str]) {
    let repo_v1 = prefix.join("repo/v1");
    std::fs::create_dir_all(&repo_v1).expect("local repo");
    let mut index = String::from("schema_version = 2\n");
    for component in components {
        index.push_str(&format!(
            "\n[[components]]\nname = \"{component}\"\ntargets = [{{ os = \"{os}\", arch = \"{arch}\" }}]\n",
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
        ));
    }
    std::fs::write(repo_v1.join("components-v2.toml"), index).expect("component index");
    let etc = prefix.join("etc/anolisa");
    std::fs::create_dir_all(&etc).expect("config dir");
    std::fs::write(
        etc.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"file://{}\"\n",
            repo_v1.display()
        ),
    )
    .expect("repo config");
}

fn plant_state(prefix: &std::path::Path, claimed_component: &str) {
    let layout = FsLayout::system(Some(prefix.to_path_buf()));
    std::fs::create_dir_all(&layout.state_dir).expect("state dir");
    InstalledState {
        install_mode: StateInstallMode::System,
        prefix: layout.prefix.clone(),
        objects: vec![rpm_object(TARGET), rpm_object(OTHER)],
        adapter_claims: vec![sample_claim(claimed_component)],
        ..InstalledState::default()
    }
    .save(&layout.state_dir.join("installed.toml"))
    .expect("state");
}

fn rpm_object(component: &str) -> InstalledObject {
    InstalledObject {
        kind: ObjectKind::Component,
        name: component.to_string(),
        version: "1.0.0-1".to_string(),
        status: ObjectStatus::Adopted,
        manifest_digest: None,
        distribution_source: None,
        raw_package: None,
        install_backend: Some("rpm".to_string()),
        ownership: Some(Ownership::RpmObserved),
        rpm_metadata: Some(RpmMetadata {
            package_name: component.to_string(),
            evr: Some("1.0.0-1".to_string()),
            arch: Some("x86_64".to_string()),
            source_repo: Some("@System".to_string()),
        }),
        installed_at: "2026-01-01T00:00:00Z".to_string(),
        last_operation_id: None,
        managed: false,
        adopted: true,
        subscription_scope: SubscriptionScope::None,
        enabled_features: Vec::new(),
        component_refs: Vec::new(),
        files: Vec::new(),
        external_modified_files: Vec::new(),
        services: Vec::new(),
        health: Vec::new(),
        provisioned_packages: Vec::new(),
    }
}

fn sample_claim(component: &str) -> AdapterClaim {
    AdapterClaim {
        claim_schema: CLAIM_SCHEMA_VERSION,
        component: component.to_string(),
        framework: FRAMEWORK.to_string(),
        plugin_id: None,
        adapter_type: None,
        enabled_at: "2026-01-01T00:00:00Z".to_string(),
        resource_root: PathBuf::from("/tmp/anolisa-forget-dry-run"),
        bundle_digest: None,
        source_revision: None,
        materialized_files: Vec::new(),
        driver_schema: DRIVER_SCHEMA_VERSION,
        status: ClaimStatus::Enabled,
        notices: Vec::new(),
        resources: Vec::new(),
        driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
            state_dir_resource: "state".to_string(),
            plugin_resource: "plugin".to_string(),
            skill_resources: Vec::new(),
            config_resources: Vec::new(),

            displaced_plugins: Vec::new(),
        }),
    }
}

fn parse_error(output: &Output) -> Value {
    assert_eq!(
        Some(2),
        output.status.code(),
        "adapter refusal must exit 2; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be a JSON envelope");
    assert_eq!(value.get("ok"), Some(&Value::Bool(false)), "{value}");
    let command = format!("forget {TARGET}");
    assert_eq!(
        value.get("command").and_then(Value::as_str),
        Some(command.as_str()),
        "{value}"
    );
    let error = value.get("error").expect("error object").clone();
    assert_eq!(
        error.get("code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{value}"
    );
    let reason = error
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        reason.contains("adapter disable") && reason.contains(FRAMEWORK),
        "reason must name adapter disable and {FRAMEWORK}: {value}"
    );
    value
}

#[test]
fn forget_dry_run_json_refuses_enabled_adapter() {
    let fixture = ForgetFixture::with_claim_on(TARGET);
    parse_error(&fixture.run_dry_run());
}

#[test]
fn forget_dry_run_json_ignores_unrelated_adapter_claim() {
    let fixture = ForgetFixture::with_claim_on(OTHER);
    let output = fixture.run_dry_run();
    assert_eq!(
        Some(0),
        output.status.code(),
        "unrelated receipt must not block this dry-run; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be a JSON envelope");
    assert_eq!(value.get("ok"), Some(&Value::Bool(true)), "{value}");
    let data = value.get("data").expect("data");
    assert_eq!(data.get("component").and_then(Value::as_str), Some(TARGET));
    assert_eq!(
        data.get("provenance").and_then(Value::as_str),
        Some("adopted")
    );
    assert_eq!(
        data.get("install_mode").and_then(Value::as_str),
        Some("system")
    );
    assert_eq!(data.get("dry_run"), Some(&Value::Bool(true)));
    assert_eq!(data.get("forgotten"), Some(&Value::Bool(false)));
    assert!(data.get("operation_id").is_none(), "{value}");
}
