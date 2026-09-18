//! Subprocess coverage for global `--quiet` on adapter read-only printers.
//!
//! `adapter scan` already hid warnings under `--quiet` while still printing
//! the empty-state line and table. `adapter status` never consulted the flag.
//! Both must stay silent without `--json`, and `--json --quiet` must still
//! emit the standard envelope. Subprocesses pin `--install-mode system` and
//! a temporary `--prefix` so host receipts under `/var/lib/anolisa` and
//! packaged adapters cannot change empty-state or quiet assertions.

use std::path::PathBuf;
use std::process::Output;

use anolisa_core::adapter::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus, CoshClaim,
    DRIVER_SCHEMA_VERSION, DriverPayload,
};
use anolisa_core::{
    FileOwner, InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind,
    ObjectStatus, OwnedFile, OwnedFileKind, Ownership, SubscriptionScope,
};
use anolisa_platform::fs_layout::FsLayout;
use serde_json::Value;

mod common;

const COMPONENT: &str = "quiet-demo";
const FRAMEWORK: &str = "cosh";

const MANIFEST: &str = r#"[component]
name = "quiet-demo"
version = "0.1.0"

[[adapters]]
framework = "cosh"
adapter_type = "extension"
source = "adapters/quiet-demo/cosh"
dest = "{datadir}/adapters/{component}/cosh/"
"#;

struct QuietFixture {
    _tmp: tempfile::TempDir,
    prefix: PathBuf,
    home: PathBuf,
    data_home: PathBuf,
    config_home: PathBuf,
    state_home: PathBuf,
    cache_home: PathBuf,
    runtime_dir: PathBuf,
}

impl QuietFixture {
    fn empty() -> Self {
        Self::new(false)
    }

    fn populated() -> Self {
        Self::new(true)
    }

    fn new(populate: bool) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let prefix = root.join("system");
        let home = root.join("home");
        let data_home = root.join("xdg-data");
        let config_home = root.join("xdg-config");
        let state_home = root.join("xdg-state");
        let cache_home = root.join("xdg-cache");
        let runtime_dir = root.join("xdg-runtime");
        if populate {
            plant_entries(&prefix, &home);
        }
        Self {
            _tmp: tmp,
            prefix,
            home,
            data_home,
            config_home,
            state_home,
            cache_home,
            runtime_dir,
        }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        let prefix = self.prefix.to_string_lossy().into_owned();
        let mut args = vec!["--install-mode", "system", "--prefix", prefix.as_str()];
        args.extend_from_slice(arguments);
        common::run_with_path_env(
            &args,
            &[
                ("HOME", self.home.as_path()),
                ("XDG_DATA_HOME", self.data_home.as_path()),
                ("XDG_CONFIG_HOME", self.config_home.as_path()),
                ("XDG_STATE_HOME", self.state_home.as_path()),
                ("XDG_CACHE_HOME", self.cache_home.as_path()),
                ("XDG_RUNTIME_DIR", self.runtime_dir.as_path()),
            ],
        )
    }
}

fn plant_entries(prefix: &std::path::Path, home: &std::path::Path) {
    let layout = FsLayout::system(Some(prefix.to_path_buf()));
    let resource_root = layout
        .datadir
        .join("adapters")
        .join(COMPONENT)
        .join(FRAMEWORK);
    std::fs::create_dir_all(&resource_root).expect("resource root");
    let extension_manifest = format!(r#"{{"id":"{COMPONENT}","name":"Quiet Demo"}}"#);
    let resource_manifest = resource_root.join("cosh-extension.json");
    std::fs::write(&resource_manifest, extension_manifest.as_bytes()).expect("resource manifest");

    let snapshot = layout.snapshot_path(COMPONENT);
    std::fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("snapshot dir");
    std::fs::write(&snapshot, MANIFEST).expect("component snapshot");

    let extension_dir = home
        .join(".copilot-shell")
        .join("extensions")
        .join(COMPONENT);
    std::fs::create_dir_all(&extension_dir).expect("cosh extension dir");
    std::fs::write(
        extension_dir.join("cosh-extension.json"),
        extension_manifest.as_bytes(),
    )
    .expect("delivered manifest");
    std::fs::write(
        extension_dir.join(".anolisa-adapter"),
        b"ANOLISA-managed cosh extension\n",
    )
    .expect("ownership marker");

    let mut installed = component(COMPONENT);
    installed.files.push(OwnedFile {
        path: resource_manifest,
        owner: FileOwner::Anolisa,
        sha256: None,
        kind: OwnedFileKind::File,
        referent: None,
        mode: None,
        capabilities: Vec::new(),
    });
    std::fs::create_dir_all(&layout.state_dir).expect("state dir");
    std::fs::write(
        layout.state_dir.join("installed.toml"),
        toml::to_string_pretty(&InstalledState {
            install_mode: StateInstallMode::System,
            prefix: layout.prefix.clone(),
            objects: vec![installed],
            adapter_claims: vec![AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: COMPONENT.to_string(),
                framework: FRAMEWORK.to_string(),
                plugin_id: Some(COMPONENT.to_string()),
                adapter_type: Some("extension".to_string()),
                enabled_at: "2026-01-01T00:00:00Z".to_string(),
                resource_root,
                bundle_digest: None,
                source_revision: None,
                materialized_files: Vec::new(),
                driver_schema: DRIVER_SCHEMA_VERSION,
                status: ClaimStatus::Enabled,
                notices: Vec::new(),
                resources: vec![ClaimResource {
                    id: "cosh_extension_dir".to_string(),
                    purpose: "cosh_extension_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: extension_dir,
                    },
                }],
                driver_payload: DriverPayload::Cosh(CoshClaim {
                    extension_dir_resource: "cosh_extension_dir".to_string(),
                }),
            }],
            ..InstalledState::default()
        })
        .expect("legacy fixture"),
    )
    .expect("state");
}

fn component(name: &str) -> InstalledObject {
    InstalledObject {
        kind: ObjectKind::Component,
        name: name.to_string(),
        version: "0.1.0".to_string(),
        status: ObjectStatus::Installed,
        manifest_digest: None,
        distribution_source: None,
        raw_package: None,
        install_backend: Some("raw".to_string()),
        ownership: Some(Ownership::RawManaged),
        rpm_metadata: None,
        installed_at: "2026-01-01T00:00:00Z".to_string(),
        last_operation_id: None,
        managed: true,
        adopted: false,
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert_eq!(
        Some(0),
        output.status.code(),
        "unexpected exit; stdout: {}; stderr: {}",
        stdout(output),
        stderr(output),
    );
}

fn parse_envelope(output: &Output, command: &str) -> Value {
    assert_success(output);
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be a JSON envelope");
    assert_eq!(value.get("ok"), Some(&Value::Bool(true)));
    assert_eq!(value.get("command").and_then(Value::as_str), Some(command));
    value
}

#[test]
fn adapter_scan_quiet_suppresses_human_stdout() {
    let fixture = QuietFixture::empty();
    let output = fixture.run(&["--quiet", "--no-color", "adapter", "scan"]);
    assert_success(&output);
    assert!(
        output.stdout.is_empty(),
        "--quiet adapter scan must not print the table or empty-state line; stdout: {}",
        stdout(&output)
    );
    assert!(
        !stderr(&output).contains("No adapter declarations"),
        "empty-state text belongs on stdout, not stderr: {}",
        stderr(&output)
    );
}

#[test]
fn adapter_scan_without_quiet_prints_human_empty_state() {
    let fixture = QuietFixture::empty();
    let output = fixture.run(&["--no-color", "adapter", "scan"]);
    assert_success(&output);
    assert!(
        stdout(&output).contains("No adapter declarations or resources found."),
        "human scan should report the empty state; stdout: {}",
        stdout(&output)
    );
}

#[test]
fn adapter_scan_quiet_json_still_emits_envelope() {
    let fixture = QuietFixture::empty();
    let output = fixture.run(&["--quiet", "--json", "adapter", "scan"]);
    let value = parse_envelope(&output, "adapter scan");
    assert!(
        value["data"]["adapters"].is_array(),
        "JSON scan payload must keep adapters under data: {value}"
    );
}

#[test]
fn adapter_status_quiet_suppresses_human_stdout() {
    let fixture = QuietFixture::empty();
    let output = fixture.run(&["--quiet", "--no-color", "adapter", "status"]);
    assert_success(&output);
    assert!(
        output.stdout.is_empty(),
        "--quiet adapter status must not print receipts or the empty-state line; stdout: {}",
        stdout(&output)
    );
}

#[test]
fn adapter_status_without_quiet_prints_human_empty_state() {
    let fixture = QuietFixture::empty();
    let output = fixture.run(&["--no-color", "adapter", "status"]);
    assert_success(&output);
    assert!(
        stdout(&output).contains("No adapter receipts."),
        "human status should report the empty state; stdout: {}",
        stdout(&output)
    );
}

#[test]
fn adapter_status_quiet_json_still_emits_envelope() {
    let fixture = QuietFixture::empty();
    let output = fixture.run(&["--quiet", "--json", "adapter", "status"]);
    let value = parse_envelope(&output, "adapter status");
    assert!(
        value["data"]["receipts"].is_array(),
        "JSON status payload must keep receipts under data: {value}"
    );
}

#[test]
fn adapter_scan_quiet_suppresses_populated_table() {
    let fixture = QuietFixture::populated();
    let human = fixture.run(&["--no-color", "adapter", "scan"]);
    assert_success(&human);
    assert!(
        stdout(&human).contains(COMPONENT),
        "planted adapter must appear without --quiet; stdout: {}",
        stdout(&human)
    );
    assert!(
        !stdout(&human).contains("No adapter declarations or resources found."),
        "populated scan must not use the empty-state line; stdout: {}",
        stdout(&human)
    );

    let quiet = fixture.run(&["--quiet", "--no-color", "adapter", "scan"]);
    assert_success(&quiet);
    assert!(
        quiet.stdout.is_empty(),
        "--quiet adapter scan must suppress the planted table; stdout: {}",
        stdout(&quiet)
    );
}

#[test]
fn adapter_status_quiet_suppresses_populated_receipts() {
    let fixture = QuietFixture::populated();
    let human = fixture.run(&["--no-color", "adapter", "status"]);
    assert_success(&human);
    assert!(
        stdout(&human).contains(&format!("{COMPONENT}/{FRAMEWORK}")),
        "planted receipt must appear without --quiet; stdout: {}",
        stdout(&human)
    );
    assert!(
        !stdout(&human).contains("No adapter receipts."),
        "populated status must not use the empty-state line; stdout: {}",
        stdout(&human)
    );

    let quiet = fixture.run(&["--quiet", "--no-color", "adapter", "status"]);
    assert_success(&quiet);
    assert!(
        quiet.stdout.is_empty(),
        "--quiet adapter status must suppress planted receipts; stdout: {}",
        stdout(&quiet)
    );
}

#[test]
fn adapter_quiet_json_keeps_populated_payload() {
    let fixture = QuietFixture::populated();
    let scan = parse_envelope(
        &fixture.run(&["--quiet", "--json", "adapter", "scan"]),
        "adapter scan",
    );
    let adapters = scan["data"]["adapters"]
        .as_array()
        .expect("JSON scan payload must keep adapters under data");
    assert!(
        adapters
            .iter()
            .any(|row| row["component"].as_str() == Some(COMPONENT)),
        "quiet JSON scan must still list the planted adapter: {scan}"
    );

    let status = parse_envelope(
        &fixture.run(&["--quiet", "--json", "adapter", "status"]),
        "adapter status",
    );
    let receipts = status["data"]["receipts"]
        .as_array()
        .expect("JSON status payload must keep receipts under data");
    assert!(
        receipts
            .iter()
            .any(|row| row["component"].as_str() == Some(COMPONENT)),
        "quiet JSON status must still list the planted receipt: {status}"
    );
}
