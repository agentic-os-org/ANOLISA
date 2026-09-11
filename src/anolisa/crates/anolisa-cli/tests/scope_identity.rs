//! Subprocess coverage for component identity across user and system scopes.

use std::path::{Path, PathBuf};
use std::process::Output;

use anolisa_core::adapter::claim::{AdapterClaim, ClaimStatus, DriverPayload, OpenClawClaim};
use anolisa_core::domain::ProviderBinding;
use anolisa_core::state_store::StateStore;
use anolisa_core::transaction::{Transaction, TransactionOutcomeStatus, TransactionStep};
use anolisa_core::{
    FileOwner, InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind,
    ObjectStatus, OwnedFile, OwnedFileKind, Ownership, SubscriptionScope,
};
use anolisa_platform::fs_layout::FsLayout;

mod common;

struct ScopeFixture {
    _tmp: tempfile::TempDir,
    system_prefix: PathBuf,
    home: PathBuf,
    data_home: PathBuf,
    config_home: PathBuf,
    state_home: PathBuf,
    cache_home: PathBuf,
    runtime_dir: PathBuf,
    user_state_path: PathBuf,
    system_state_path: PathBuf,
    owned_file: PathBuf,
    manifest_marker: PathBuf,
}

#[derive(Clone, Copy)]
enum SystemStateFailure {
    FutureSchema,
    InvalidToml,
    ReadFailure,
}

impl ScopeFixture {
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

        let owned_file = user_layout.bin_dir.join("cosh");
        std::fs::create_dir_all(&user_layout.bin_dir).expect("user bin dir");
        std::fs::write(&owned_file, "user-cosh-binary").expect("owned file");
        let mut user_component = component("cosh");
        user_component.files.push(OwnedFile {
            path: owned_file.clone(),
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        });
        write_state(&user_layout, StateInstallMode::User, vec![user_component]);
        write_state(
            &system_layout,
            StateInstallMode::System,
            vec![quarantined_component("legacy-name")],
        );
        let manifest_marker = user_layout
            .state_dir
            .join("component-manifests/cosh/fixture-marker");
        std::fs::create_dir_all(manifest_marker.parent().expect("manifest parent"))
            .expect("manifest dir");
        std::fs::write(&manifest_marker, "user-cosh-manifest").expect("manifest marker");
        seed_alias_index(root, &user_layout);

        let user_state_path = user_layout.state_dir.join("installed.toml");
        let system_state_path = system_layout.state_dir.join("installed.toml");

        Self {
            _tmp: tmp,
            system_prefix,
            home,
            data_home,
            config_home,
            state_home,
            cache_home,
            runtime_dir,
            user_state_path,
            system_state_path,
            owned_file,
            manifest_marker,
        }
    }

    fn run(&self, command: &str) -> Output {
        self.run_with_dry_run(command, true)
    }

    fn run_with_dry_run(&self, command: &str, dry_run: bool) -> Output {
        self.run_args(&[command, "legacy-name"], dry_run)
    }

    fn run_args(&self, command_args: &[&str], dry_run: bool) -> Output {
        let prefix = self.system_prefix.to_string_lossy();
        let mut args = vec!["--json"];
        if dry_run {
            args.push("--dry-run");
        }
        args.extend(["--install-mode", "user", "--prefix", &prefix]);
        args.extend_from_slice(command_args);
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

    fn run_human_forget(&self, quiet: bool) -> Output {
        let prefix = self.system_prefix.to_string_lossy();
        let mut args = vec!["--no-color"];
        if quiet {
            args.push("--quiet");
        }
        args.extend([
            "--install-mode",
            "user",
            "--prefix",
            &prefix,
            "forget",
            "legacy-name",
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

    fn seed_user_claim(&self, component: &str) {
        let mut state = StateStore::load(
            &self.user_state_path,
            anolisa_platform::privilege::effective_uid(),
        )
        .expect("load user state");
        state.upsert_adapter_claim(adapter_claim(component));
        state.save(&self.user_state_path).expect("save user claim");
    }

    fn break_system_state(&self, failure: SystemStateFailure) {
        match failure {
            SystemStateFailure::FutureSchema => {
                let state =
                    std::fs::read_to_string(&self.system_state_path).expect("read system state");
                // 7 is one past the newest schema this CLI accepts (v6,
                // the anchored store format).
                let future = state.replacen("schema_version = 4", "schema_version = 7", 1);
                assert_ne!(future, state, "fixture must start at schema 4");
                std::fs::write(&self.system_state_path, future).expect("future system state");
            }
            SystemStateFailure::InvalidToml => {
                std::fs::write(&self.system_state_path, "invalid = [")
                    .expect("invalid system state");
            }
            SystemStateFailure::ReadFailure => {
                std::fs::remove_file(&self.system_state_path).expect("remove system state file");
                std::fs::create_dir(&self.system_state_path)
                    .expect("replace system state with unreadable file shape");
            }
        }
    }

    fn replace_system_with_package_alias(&self) {
        let layout = FsLayout::system(Some(self.system_prefix.clone()));
        let mut system_component = component("system-tool");
        system_component.raw_package = Some("legacy-name".to_string());
        write_state(&layout, StateInstallMode::System, vec![system_component]);
    }

    fn add_writable_package_owner(&self) {
        let mut state = StateStore::load(
            &self.user_state_path,
            anolisa_platform::privilege::effective_uid(),
        )
        .expect("load user state");
        let mut owner = state
            .find(ObjectKind::Component, "cosh")
            .expect("seeded cosh")
            .clone();
        owner.name = "user-tool".to_string();
        let ProviderBinding::Owned { artifact } = &mut owner.binding else {
            panic!("fixture must be owned");
        };
        artifact.raw_package = Some("legacy-name".to_string());
        artifact.files.clear();
        state.upsert(owner);
        state
            .save(&self.user_state_path)
            .expect("save package owner");

        let layout = FsLayout::system(Some(self.system_prefix.clone()));
        write_state(&layout, StateInstallMode::System, Vec::new());
    }

    fn user_artifacts(&self) -> [Vec<u8>; 3] {
        [
            std::fs::read(&self.user_state_path).expect("user state"),
            std::fs::read(&self.owned_file).expect("owned file"),
            std::fs::read(&self.manifest_marker).expect("manifest marker"),
        ]
    }

    fn begin_fresh_user_install_journal(&self, subject: &str) -> PathBuf {
        let layout = FsLayout::user_with_overrides(
            self.home.clone(),
            Some(self.data_home.clone()),
            Some(self.config_home.clone()),
            Some(self.state_home.clone()),
            Some(self.cache_home.clone()),
            Some(self.runtime_dir.clone()),
        );
        let mut journal = Transaction::begin_with_subject(
            "install",
            Some(subject),
            self.user_state_path.clone(),
            &layout.state_dir.join("journal"),
        )
        .expect("begin fresh user install journal");
        journal
            .record_steps([TransactionStep::planned(
                "files",
                "owned-files",
                "place-files",
                None,
            )])
            .expect("record install step");
        journal.journal_path.clone()
    }
}

fn write_state(layout: &FsLayout, mode: StateInstallMode, objects: Vec<InstalledObject>) {
    std::fs::create_dir_all(&layout.state_dir).expect("state dir");
    InstalledState {
        install_mode: mode,
        prefix: layout.prefix.clone(),
        objects,
        ..InstalledState::default()
    }
    .save(&layout.state_dir.join("installed.toml"))
    .expect("state");
}

fn component(name: &str) -> InstalledObject {
    InstalledObject {
        kind: ObjectKind::Component,
        name: name.to_string(),
        version: "1.0.0".to_string(),
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

fn quarantined_component(name: &str) -> InstalledObject {
    let mut object = component(name);
    object.install_backend = None;
    object.ownership = None;
    object.managed = false;
    object
}

fn adapter_claim(component: &str) -> AdapterClaim {
    AdapterClaim {
        claim_schema: 1,
        component: component.to_string(),
        framework: "openclaw".to_string(),
        plugin_id: None,
        adapter_type: None,
        enabled_at: "2026-01-01T00:00:00Z".to_string(),
        resource_root: PathBuf::from("/tmp/adapter-resource"),
        bundle_digest: None,
        source_revision: None,
        materialized_files: Vec::new(),
        driver_schema: 1,
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

fn seed_alias_index(root: &Path, user_layout: &FsLayout) {
    let repo_v1 = root.join("repo").join("v1");
    std::fs::create_dir_all(&repo_v1).expect("repo dir");
    let env = anolisa_env::EnvService::detect();
    std::fs::write(
        repo_v1.join("index.toml"),
        format!(
            r#"schema_version = 1
channel = "stable"
publisher = "test"

[[entries]]
component = "legacy-name"
version = "1.0.0"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "legacy-name.tar.gz"
os = "{}"
arch = "{}"
install_modes = ["user"]
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"

[[entries]]
component = "cosh"
version = "1.0.0"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "cosh.tar.gz"
os = "{}"
arch = "{}"
install_modes = ["user"]
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
"#,
            env.os, env.arch, env.os, env.arch,
        ),
    )
    .expect("distribution index");
    std::fs::write(
        repo_v1.join("components-v2.toml"),
        r#"
schema_version = 2

[[components]]
name = "cosh"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.backends]]
kind = "raw"
package = "cosh"

[[components.aliases]]
kind = "rpm-package"
name = "legacy-name"
"#,
    )
    .expect("component index");
    std::fs::create_dir_all(&user_layout.etc_dir).expect("config dir");
    std::fs::write(
        user_layout.etc_dir.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n[backends.raw]\nbase_url = \"file://{}\"\n",
            repo_v1.display()
        ),
    )
    .expect("repo config");
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be JSON: {error}; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

#[test]
fn forget_exact_system_identity_does_not_mutate_user_alias_target() {
    let fixture = ScopeFixture::new();
    let output = fixture.run("forget");
    let envelope = json(&output);

    assert!(!output.status.success(), "forget must refuse: {envelope}");
    assert_eq!(envelope["error"]["code"], "PERMISSION_DENIED");
    let reason = envelope["error"]["reason"].as_str().expect("error reason");
    assert!(reason.contains("legacy-name"), "wrong identity: {reason}");
    assert!(reason.contains("system-scope"), "wrong scope: {reason}");
    assert!(!reason.contains("component 'cosh'"), "alias won: {reason}");
}

#[test]
fn forget_system_package_identity_does_not_mutate_user_repo_alias_target() {
    let fixture = ScopeFixture::new();
    fixture.replace_system_with_package_alias();
    let user_before = fixture.user_artifacts();
    let system_before = std::fs::read(&fixture.system_state_path).expect("system state");

    let output = fixture.run_with_dry_run("forget", false);
    let envelope = json(&output);

    assert!(!output.status.success(), "forget must refuse: {envelope}");
    assert_eq!(envelope["error"]["code"], "PERMISSION_DENIED");
    assert_eq!(fixture.user_artifacts(), user_before);
    assert_eq!(
        std::fs::read(&fixture.system_state_path).expect("system state"),
        system_before,
    );
}

#[test]
fn forget_writable_package_identity_does_not_mutate_repo_alias_target() {
    let fixture = ScopeFixture::new();
    fixture.add_writable_package_owner();

    let output = fixture.run_with_dry_run("forget", false);
    let envelope = json(&output);

    assert!(output.status.success(), "forget must succeed: {envelope}");
    assert_eq!(envelope["command"], "forget");
    assert_eq!(envelope["data"]["component"], "user-tool");
    assert_eq!(envelope["data"]["provenance"], "owned");
    assert_eq!(envelope["data"]["install_mode"], "user");
    assert_eq!(envelope["data"]["forgotten"], true);
    assert_eq!(envelope["data"]["dry_run"], false);
    assert!(
        envelope["data"]["operation_id"]
            .as_str()
            .is_some_and(|operation_id| operation_id.starts_with("op-forget-")),
        "missing operation id: {envelope}"
    );
    let state = StateStore::load(
        &fixture.user_state_path,
        anolisa_platform::privilege::effective_uid(),
    )
    .expect("reload user state");
    assert!(
        state.find(ObjectKind::Component, "user-tool").is_none(),
        "the package identity owner must be forgotten",
    );
    assert!(
        state.find(ObjectKind::Component, "cosh").is_some(),
        "the repository alias target must remain",
    );
    assert!(fixture.owned_file.is_file());
}

#[test]
fn forget_human_and_quiet_output_remain_compatible() {
    let human_fixture = ScopeFixture::new();
    human_fixture.add_writable_package_owner();
    let human = human_fixture.run_human_forget(false);
    assert!(
        human.status.success(),
        "human forget failed: {}",
        String::from_utf8_lossy(&human.stderr)
    );
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("✓ forgot user-tool (owned)"), "{stdout}");
    assert!(
        stdout.contains("no package operation was performed"),
        "{stdout}"
    );

    let quiet_fixture = ScopeFixture::new();
    quiet_fixture.add_writable_package_owner();
    let quiet = quiet_fixture.run_human_forget(true);
    assert!(
        quiet.status.success(),
        "quiet forget failed: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert!(quiet.stdout.is_empty(), "quiet stdout must stay empty");
}

#[test]
fn restart_exact_system_identity_does_not_operate_user_alias_target() {
    let fixture = ScopeFixture::new();
    let output = fixture.run_with_dry_run("restart", false);
    let envelope = json(&output);

    assert!(!output.status.success(), "restart must refuse: {envelope}");
    assert_eq!(envelope["error"]["code"], "PERMISSION_DENIED");
    let reason = envelope["error"]["reason"].as_str().expect("error reason");
    assert!(reason.contains("legacy-name"), "wrong identity: {reason}");
    assert!(reason.contains("system-scope"), "wrong scope: {reason}");
    assert!(!reason.contains("component 'cosh'"), "alias won: {reason}");
}

#[test]
fn adapter_enable_keeps_the_visible_system_exact_identity() {
    let fixture = ScopeFixture::new();
    let before = fixture.user_artifacts();
    let output = fixture.run_args(&["adapter", "enable", "legacy-name", "openclaw"], true);
    let envelope = json(&output);

    assert!(
        !output.status.success(),
        "quarantined source must fail: {envelope}"
    );
    let reason = envelope["error"]["reason"].as_str().expect("error reason");
    assert!(reason.contains("legacy-name"), "wrong identity: {reason}");
    assert!(!reason.contains("component 'cosh'"), "alias won: {reason}");
    assert_eq!(fixture.user_artifacts(), before);
}

#[test]
fn adapter_disable_system_exact_does_not_cleanup_user_alias_receipt() {
    let fixture = ScopeFixture::new();
    fixture.seed_user_claim("cosh");
    let before = fixture.user_artifacts();
    let output = fixture.run_args(&["adapter", "disable", "legacy-name"], false);
    let envelope = json(&output);

    assert!(
        output.status.success(),
        "exact no-op must succeed: {envelope}"
    );
    assert_eq!(envelope["data"]["component"], "legacy-name");
    assert_eq!(envelope["data"]["claim_removed"], false);
    assert_eq!(fixture.user_artifacts(), before);
}

#[test]
fn install_exact_system_identity_targets_a_fresh_user_installation() {
    let fixture = ScopeFixture::new();
    let output = fixture.run("install");
    let envelope = json(&output);

    assert_eq!(
        output.status.code(),
        Some(0),
        "user install should remain available: {envelope}; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(envelope["data"]["component"], "legacy-name");
    assert_eq!(envelope["data"]["backend"], "raw");
    assert_eq!(envelope["data"]["action"], "planned");
    assert_eq!(envelope["data"]["dry_run"], true);
}

#[test]
fn repair_uses_exact_writable_journal_identity_before_system_state() {
    let fixture = ScopeFixture::new();
    let journal_path = fixture.begin_fresh_user_install_journal("legacy-name");

    let output = fixture.run_with_dry_run("repair", false);
    let envelope = json(&output);

    assert!(
        !output.status.success(),
        "unsafe recovery must fail: {envelope}"
    );
    assert_ne!(
        envelope["error"]["code"], "PERMISSION_DENIED",
        "the writable journal must win identity resolution: {envelope}",
    );
    let reason = envelope["error"]["reason"].as_str().expect("error reason");
    assert!(reason.contains("left pending"), "wrong failure: {reason}");
    assert_eq!(
        Transaction::load_journal(&journal_path)
            .expect("reload journal")
            .status,
        TransactionOutcomeStatus::InFlight,
    );
}

#[test]
fn incomplete_system_visibility_never_mutates_the_user_alias_target() {
    for failure in [
        SystemStateFailure::FutureSchema,
        SystemStateFailure::InvalidToml,
        SystemStateFailure::ReadFailure,
    ] {
        let fixture = ScopeFixture::new();
        fixture.break_system_state(failure);
        let before = fixture.user_artifacts();

        let output = fixture.run_with_dry_run("uninstall", false);
        let envelope = json(&output);

        assert!(!output.status.success(), "uninstall must fail: {envelope}");
        let reason = envelope["error"]["reason"].as_str().expect("error reason");
        assert!(
            reason.contains("visible state is incomplete"),
            "wrong failure: {reason}"
        );
        assert!(
            reason.contains("legacy-name"),
            "lost literal input: {reason}"
        );
        assert!(!reason.contains("component 'cosh'"), "alias won: {reason}");
        assert_eq!(fixture.user_artifacts(), before);
    }
}

#[test]
fn incomplete_system_visibility_blocks_restart_alias_inference() {
    for failure in [
        SystemStateFailure::FutureSchema,
        SystemStateFailure::InvalidToml,
        SystemStateFailure::ReadFailure,
    ] {
        let fixture = ScopeFixture::new();
        fixture.break_system_state(failure);
        let before = fixture.user_artifacts();

        let output = fixture.run_with_dry_run("restart", false);
        let envelope = json(&output);

        assert!(!output.status.success(), "restart must fail: {envelope}");
        let reason = envelope["error"]["reason"].as_str().expect("error reason");
        assert!(
            reason.contains("visible state is incomplete"),
            "wrong failure: {reason}"
        );
        assert!(!reason.contains("component 'cosh'"), "alias won: {reason}");
        assert_eq!(fixture.user_artifacts(), before);
    }
}

#[test]
fn incomplete_system_visibility_blocks_adapter_alias_inference() {
    for failure in [
        SystemStateFailure::FutureSchema,
        SystemStateFailure::InvalidToml,
        SystemStateFailure::ReadFailure,
    ] {
        let fixture = ScopeFixture::new();
        fixture.seed_user_claim("cosh");
        fixture.break_system_state(failure);
        let before = fixture.user_artifacts();

        let output = fixture.run_args(&["adapter", "disable", "legacy-name"], false);
        let envelope = json(&output);

        assert!(
            !output.status.success(),
            "adapter disable must fail: {envelope}"
        );
        let reason = envelope["error"]["reason"].as_str().expect("error reason");
        assert!(
            reason.contains("visible state is incomplete"),
            "wrong failure: {reason}"
        );
        assert!(!reason.contains("component 'cosh'"), "alias won: {reason}");
        assert_eq!(fixture.user_artifacts(), before);
    }
}

#[test]
fn incomplete_system_visibility_blocks_install_alias_inference() {
    for failure in [
        SystemStateFailure::FutureSchema,
        SystemStateFailure::InvalidToml,
        SystemStateFailure::ReadFailure,
    ] {
        let fixture = ScopeFixture::new();
        fixture.break_system_state(failure);

        // `legacy-name` is an index alias of `cosh`: remapping it needs
        // complete visibility, because the unreadable system root could hold
        // an exact `legacy-name` record. The literal input is no longer
        // pinned as a new identity (issue #2630) — the install refuses.
        let output = fixture.run("install");
        let envelope = json(&output);

        assert!(
            !output.status.success(),
            "alias install must refuse under incomplete visibility: {envelope}"
        );
        let reason = envelope["error"]["reason"].as_str().expect("error reason");
        assert!(
            reason.contains("visible state is incomplete"),
            "wrong failure: {reason}"
        );

        // A canonical index name involves no remapping and stays addressable
        // even while the system root is unreadable — here it hits the
        // fixture's existing user record and reports the idempotent NoOp.
        let output = fixture.run_args(&["install", "cosh"], true);
        let envelope = json(&output);
        assert_eq!(
            output.status.code(),
            Some(0),
            "canonical user install should remain available: {envelope}; stderr: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(envelope["data"]["component"], "cosh");
        assert_eq!(envelope["data"]["action"], "already-installed");
    }
}
