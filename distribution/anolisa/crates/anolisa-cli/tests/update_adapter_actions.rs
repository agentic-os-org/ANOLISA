//! End-to-end coverage for the adapter actions a component update leaves
//! behind (issue #1885): human output, `--quiet`, `--json`, and `--dry-run`.
//!
//! The fixture installs a raw component whose adapter bundle is covered by
//! an enabled receipt, then publishes a newer artifact with a changed
//! bundle. Running the real binary through `update` must name the stale
//! adapter and its exact recovery command without touching the receipt.

use std::path::{Path, PathBuf};
use std::process::Output;

use anolisa_core::adapter::claim::{
    AdapterClaim, AdapterSourceRevision, CLAIM_SCHEMA_VERSION, ClaimStatus, CoshClaim,
    DRIVER_SCHEMA_VERSION, DriverPayload, ManagedFileKind, ManagedSourceFile,
};
use anolisa_core::state_store::StateStore;
use anolisa_core::{
    FileOwner, InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind,
    ObjectStatus, OwnedFile, OwnedFileKind, Ownership, SubscriptionScope,
};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::privilege;

mod common;

const COMPONENT: &str = "stale-demo";

fn manifest(version: &str, with_adapter: bool) -> String {
    let adapter = if with_adapter {
        format!(
            r#"
[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"
"#
        )
    } else {
        String::new()
    };
    format!(
        r#"[component]
name = "{COMPONENT}"
version = "{version}"

[component.layout]
modes = ["system", "user"]

[[component.layout.files]]
source = "bin/{COMPONENT}"
target = "{{bindir}}/{COMPONENT}"
mode = "0755"
type = "executable"
{adapter}"#
    )
}

fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::{Builder, Header};
    let enc = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(enc);
    for (path, data) in entries {
        let mut header = Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, *path, *data)
            .expect("append tar entry");
    }
    tar.into_inner()
        .expect("finish tar")
        .finish()
        .expect("finish gzip")
}

struct UpdateFixture {
    _tmp: tempfile::TempDir,
    system_prefix: PathBuf,
    home: PathBuf,
    data_home: PathBuf,
    config_home: PathBuf,
    state_home: PathBuf,
    cache_home: PathBuf,
    runtime_dir: PathBuf,
    user_layout: FsLayout,
    bundle_root: PathBuf,
    enable_revision: AdapterSourceRevision,
}

impl UpdateFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let system_prefix = root.join("system");
        let home = root.join("home");
        let data_home = root.join("xdg-data");
        let config_home = root.join("xdg-config");
        let state_home = root.join("xdg-state");
        let cache_home = root.join("xdg-cache");
        let runtime_dir = root.join("xdg-runtime");
        let user_layout = FsLayout::user_with_overrides(
            home.clone(),
            Some(data_home.clone()),
            Some(config_home.clone()),
            Some(state_home.clone()),
            Some(cache_home.clone()),
            Some(runtime_dir.clone()),
        );
        let system_layout = FsLayout::system(Some(system_prefix.clone()));

        // The v0.1.0 adapter bundle, as the original install laid it.
        let bundle_root = user_layout
            .datadir
            .join("adapters")
            .join(COMPONENT)
            .join("openclaw");
        std::fs::create_dir_all(&bundle_root).expect("bundle root");
        std::fs::write(bundle_root.join("plugin.json"), b"v1").expect("v1 bundle");
        let enable_revision = AdapterSourceRevision {
            source_root: std::fs::canonicalize(&bundle_root).expect("canonical bundle root"),
            files: vec![ManagedSourceFile {
                relative_path: PathBuf::from("plugin.json"),
                kind: ManagedFileKind::File,
                sha256: Some({
                    use sha2::{Digest, Sha256};
                    format!("{:x}", Sha256::digest(b"v1"))
                }),
                symlink_target: None,
            }],
            materialized_sources: Vec::new(),
        };

        // The installed binary and its manifest snapshot.
        std::fs::create_dir_all(&user_layout.bin_dir).expect("bin dir");
        let bin = user_layout.bin_dir.join(COMPONENT);
        std::fs::write(&bin, b"v1 binary\n").expect("write bin");
        let snapshot =
            FsLayout::component_manifest_snapshot_path(&user_layout.state_dir, COMPONENT);
        std::fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("snapshot dir");
        std::fs::write(&snapshot, manifest("0.1.0", false)).expect("write snapshot");

        // State: the component owns the binary, the snapshot, and the
        // bundle file; the receipt carries the enable-time managed revision.
        let sha = |path: &Path| {
            use sha2::{Digest, Sha256};
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(path).expect("read owned file"))
            )
        };
        let object = InstalledObject {
            kind: ObjectKind::Component,
            name: COMPONENT.to_string(),
            version: "0.1.0".to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: Some("https://repo.example/raw/stale-demo-0.1.0.tar.gz".into()),
            raw_package: None,
            install_backend: Some("raw".to_string()),
            ownership: Some(Ownership::RawManaged),
            rpm_metadata: None,
            installed_at: "2026-07-01T00:00:00Z".to_string(),
            last_operation_id: None,
            managed: true,
            adopted: false,
            subscription_scope: SubscriptionScope::None,
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: vec![
                OwnedFile {
                    path: bin.clone(),
                    owner: FileOwner::Anolisa,
                    sha256: Some(sha(&bin)),
                    kind: OwnedFileKind::File,
                    referent: None,
                    mode: None,
                    capabilities: Vec::new(),
                },
                OwnedFile {
                    path: snapshot.clone(),
                    owner: FileOwner::Anolisa,
                    sha256: None,
                    kind: OwnedFileKind::File,
                    referent: None,
                    mode: None,
                    capabilities: Vec::new(),
                },
                OwnedFile {
                    path: bundle_root.join("plugin.json"),
                    owner: FileOwner::Anolisa,
                    sha256: Some(sha(&bundle_root.join("plugin.json"))),
                    kind: OwnedFileKind::File,
                    referent: None,
                    mode: None,
                    capabilities: Vec::new(),
                },
            ],
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        };
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: COMPONENT.to_string(),
            framework: "openclaw".to_string(),
            plugin_id: None,
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-07-01T00:00:00Z".to_string(),
            resource_root: bundle_root.clone(),
            bundle_digest: None,
            source_revision: Some(enable_revision.clone()),
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: Vec::new(),
            driver_payload: DriverPayload::Cosh(CoshClaim {
                extension_dir_resource: "cosh_extension_dir".to_string(),
            }),
        };
        std::fs::create_dir_all(&user_layout.state_dir).expect("state dir");
        std::fs::write(
            user_layout.state_dir.join("installed.toml"),
            toml::to_string_pretty(&InstalledState {
                install_mode: StateInstallMode::User,
                prefix: user_layout.prefix.clone(),
                objects: vec![object],
                adapter_claims: vec![claim],
                ..InstalledState::default()
            })
            .expect("legacy fixture"),
        )
        .expect("user state");
        write_state(&system_layout, StateInstallMode::System, Vec::new());

        // A raw repo publishing v0.2.0 with a changed bundle.
        let artifact = tar_gz(&[
            (
                ".anolisa/component.toml",
                manifest("0.2.0", true).as_bytes(),
            ),
            (format!("bin/{COMPONENT}").as_str(), b"v2 binary\n"),
            (
                format!("adapters/{COMPONENT}/openclaw/plugin.json").as_str(),
                b"v2",
            ),
        ]);
        publish_raw_repo(&root.join("repo"), &user_layout, "0.2.0", &artifact);

        Self {
            _tmp: tmp,
            system_prefix,
            home,
            data_home,
            config_home,
            state_home,
            cache_home,
            runtime_dir,
            user_layout,
            bundle_root,
            enable_revision,
        }
    }

    fn run_update(&self, flags: &[&str]) -> Output {
        let prefix = self.system_prefix.to_string_lossy();
        let mut args: Vec<&str> = Vec::new();
        args.extend_from_slice(flags);
        args.extend([
            "--install-mode",
            "user",
            "--prefix",
            &prefix,
            "update",
            COMPONENT,
        ]);
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

    fn load_store(&self) -> StateStore {
        StateStore::load(
            &self.user_layout.state_dir.join("installed.toml"),
            privilege::effective_uid(),
        )
        .expect("load state")
    }

    /// The persisted receipt's enable-time managed revision.
    fn persisted_source_revision(&self) -> Option<AdapterSourceRevision> {
        self.load_store()
            .find_adapter_claim(COMPONENT, "openclaw")
            .expect("receipt must survive")
            .source_revision
            .clone()
    }
}

fn write_state(layout: &FsLayout, mode: StateInstallMode, objects: Vec<InstalledObject>) {
    std::fs::create_dir_all(&layout.state_dir).expect("state dir");
    std::fs::write(
        layout.state_dir.join("installed.toml"),
        toml::to_string_pretty(&InstalledState {
            install_mode: mode,
            prefix: layout.prefix.clone(),
            objects,
            ..InstalledState::default()
        })
        .expect("legacy fixture"),
    )
    .expect("state");
}

fn publish_raw_repo(root: &Path, layout: &FsLayout, version: &str, artifact: &[u8]) {
    use sha2::{Digest, Sha256};
    // `base_url` must point at the `v1/` distribution root that holds
    // `index.toml` (repo_config::raw_root).
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dir");
    let artifact_name = format!("{COMPONENT}.tar.gz");
    std::fs::write(v1.join(&artifact_name), artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(artifact));
    let env = anolisa_env::EnvService::detect();
    let index = format!(
        r#"schema_version = 1
channel = "stable"
publisher = "test"

[[entries]]
component = "{COMPONENT}"
version = "{version}"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{artifact_name}"
os = "{os}"
arch = "{arch}"
install_modes = ["system", "user"]
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(v1.join("index.toml"), index).expect("write index");
    std::fs::create_dir_all(&layout.etc_dir).expect("etc dir");
    std::fs::write(
        layout.etc_dir.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"file://{}\"\n",
            v1.display()
        ),
    )
    .expect("write repo.toml");
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn update_human_lists_stale_adapter_with_recovery_command() {
    let fixture = UpdateFixture::new();
    let output = fixture.run_update(&[]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(out.contains("stale-demo/openclaw"), "{out}");
    assert!(
        out.contains("adapter source revision changed since enable"),
        "{out}"
    );
    assert!(
        out.contains("anolisa adapter enable stale-demo openclaw"),
        "the notice must carry the exact recovery command: {out}"
    );
    // The update itself replaced the bundle ...
    assert_eq!(
        std::fs::read(fixture.bundle_root.join("plugin.json")).expect("read bundle"),
        b"v2"
    );
    // ... but the receipt keeps its enable-time revision: only an explicit
    // `adapter enable` may clear the drift.
    assert_eq!(
        fixture.persisted_source_revision().as_ref(),
        Some(&fixture.enable_revision),
        "update must report the drift, not erase it"
    );
}

#[test]
fn update_quiet_suppresses_adapter_notices() {
    let fixture = UpdateFixture::new();
    let output = fixture.run_update(&["--quiet"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        !out.contains("source revision changed") && !out.contains("adapter enable"),
        "--quiet must not print adapter notices: {out}"
    );
}

#[test]
fn update_json_carries_adapter_actions() {
    let fixture = UpdateFixture::new();
    let output = fixture.run_update(&["--json"]);
    assert_success(&output);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["updated"], true);
    assert_eq!(
        envelope["data"]["adapter_actions"],
        serde_json::json!([{
            "component": "stale-demo",
            "framework": "openclaw",
            "reason": "adapter source revision changed since enable",
            "command": "anolisa adapter enable stale-demo openclaw",
        }]),
        "{envelope}"
    );
    assert_eq!(
        fixture.persisted_source_revision().as_ref(),
        Some(&fixture.enable_revision),
        "the receipt must survive the update unchanged"
    );
}

#[test]
fn update_dry_run_reports_no_adapter_actions_and_changes_nothing() {
    let fixture = UpdateFixture::new();
    let output = fixture.run_update(&["--json", "--dry-run"]);
    assert_success(&output);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["data"]["dry_run"], true);
    assert_eq!(envelope["data"]["updated"], false);
    assert_eq!(
        envelope["data"]["adapter_actions"],
        serde_json::json!([]),
        "dry-run must not claim a post-update mismatch has occurred: {envelope}"
    );
    assert_eq!(
        std::fs::read(fixture.bundle_root.join("plugin.json")).expect("read bundle"),
        b"v1",
        "dry-run must not touch the bundle"
    );
    assert_eq!(
        fixture.persisted_source_revision().as_ref(),
        Some(&fixture.enable_revision)
    );
}
