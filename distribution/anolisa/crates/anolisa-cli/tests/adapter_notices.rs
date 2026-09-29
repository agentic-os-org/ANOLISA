//! End-to-end coverage for adapter operation notices across human, `--json`,
//! `--quiet`, and `--dry-run` output.
//!
//! Uses the `cosh` extension driver because it needs no external CLI:
//! detection succeeds when the cosh home directory exists, and enable/disable
//! are pure filesystem operations. That keeps this test hermetic while still
//! driving the real binary through the full enable/disable path.

use std::path::PathBuf;
use std::process::Output;

use anolisa_core::{
    FileOwner, InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind,
    ObjectStatus, OwnedFile, OwnedFileKind, Ownership, SubscriptionScope,
};
use anolisa_platform::fs_layout::FsLayout;
use sha2::{Digest, Sha256};

mod common;

const COMPONENT: &str = "notice-demo";

/// Component contract declaring one `post_enable` and one `post_disable`
/// notice on a cosh extension adapter.
const MANIFEST: &str = r#"[component]
name = "notice-demo"
version = "0.1.0"

[[adapters]]
framework = "cosh"
adapter_type = "extension"
source = "adapters/notice-demo/cosh"
dest = "{datadir}/adapters/{component}/cosh/"

[[adapters.notices]]
when = "post_enable"
level = "info"
text = "Start a new shell to load the extension."
command = "cosh --version"

[[adapters.notices]]
when = "post_disable"
level = "warning"
text = "Extension files were removed from the shell."
"#;

struct NoticeFixture {
    _tmp: tempfile::TempDir,
    system_prefix: PathBuf,
    home: PathBuf,
    data_home: PathBuf,
    config_home: PathBuf,
    state_home: PathBuf,
    cache_home: PathBuf,
    runtime_dir: PathBuf,
}

impl NoticeFixture {
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

        // The cosh framework counts as detected when its home exists.
        std::fs::create_dir_all(home.join(".copilot-shell")).expect("cosh home");

        // Adapter resource bundle with the cosh extension manifest.
        let resource_root = user_layout
            .datadir
            .join("adapters")
            .join(COMPONENT)
            .join("cosh");
        std::fs::create_dir_all(&resource_root).expect("resource root");
        let extension_manifest = format!(r#"{{"id":"{COMPONENT}","name":"Notice Demo"}}"#);
        let extension_manifest_path = resource_root.join("cosh-extension.json");
        std::fs::write(&extension_manifest_path, extension_manifest.as_bytes())
            .expect("cosh extension manifest");

        // Component contract snapshot carrying the notices.
        let manifest_path = user_layout.snapshot_path(COMPONENT);
        std::fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
            .expect("manifest dir");
        std::fs::write(&manifest_path, MANIFEST).expect("component manifest");

        // Component installed in the user scope; empty system scope.
        let mut installed = component(COMPONENT);
        installed.files.push(OwnedFile {
            path: extension_manifest_path,
            owner: FileOwner::Anolisa,
            sha256: Some(format!(
                "{:x}",
                Sha256::digest(extension_manifest.as_bytes())
            )),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        });
        write_state(&user_layout, StateInstallMode::User, vec![installed]);
        write_state(&system_layout, StateInstallMode::System, Vec::new());

        Self {
            _tmp: tmp,
            system_prefix,
            home,
            data_home,
            config_home,
            state_home,
            cache_home,
            runtime_dir,
        }
    }

    /// Run `anolisa [flags] adapter <sub_args>` in user mode against this
    /// fixture's temp roots.
    fn run_adapter(&self, flags: &[&str], sub_args: &[&str]) -> Output {
        let prefix = self.system_prefix.to_string_lossy();
        let mut args: Vec<&str> = Vec::new();
        args.extend_from_slice(flags);
        args.extend(["--install-mode", "user", "--prefix", &prefix]);
        args.push("adapter");
        args.extend_from_slice(sub_args);
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

    fn enable(&self, flags: &[&str]) -> Output {
        self.run_adapter(flags, &["enable", COMPONENT, "cosh"])
    }

    fn disable(&self, flags: &[&str]) -> Output {
        self.run_adapter(flags, &["disable", COMPONENT, "cosh"])
    }

    /// The delivered cosh extension directory.
    fn extension_dir(&self) -> PathBuf {
        self.home
            .join(".copilot-shell")
            .join("extensions")
            .join(COMPONENT)
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

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn enable_human_shows_post_enable_notices() {
    let fixture = NoticeFixture::new();
    let output = fixture.enable(&[]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(out.contains("Enabled notice-demo/cosh."), "{out}");
    assert!(out.contains("notices:"), "{out}");
    assert!(
        out.contains("[info] Start a new shell to load the extension."),
        "{out}"
    );
    assert!(out.contains("command: cosh --version"), "{out}");
    // The post_disable notice must not appear on enable.
    assert!(!out.contains("Extension files were removed"), "{out}");
}

#[test]
fn enable_escapes_terminal_controls_only_in_human_output() {
    let fixture = NoticeFixture::new();
    let manifest_path = FsLayout::user_with_overrides(
        fixture.home.clone(),
        Some(fixture.data_home.clone()),
        Some(fixture.config_home.clone()),
        Some(fixture.state_home.clone()),
        Some(fixture.cache_home.clone()),
        Some(fixture.runtime_dir.clone()),
    )
    .snapshot_path(COMPONENT);
    let manifest = MANIFEST
        .replace(
            "Start a new shell to load the extension.",
            r"Set title: \u001b]2;spoofed-title\u0007",
        )
        .replace("cosh --version", r"cosh \u001b[31m--version");
    std::fs::write(&manifest_path, manifest).expect("unsafe notice manifest");

    let human = fixture.enable(&["--dry-run"]);
    assert_success(&human);
    let out = stdout(&human);
    assert!(
        !out.contains('\u{1b}'),
        "ESC must not reach stdout: {out:?}"
    );
    assert!(!out.contains('\u{7}'), "BEL must not reach stdout: {out:?}");
    assert!(
        out.contains(r"[info] Set title: \u{1b}]2;spoofed-title\u{7}"),
        "{out:?}"
    );
    assert!(
        out.contains(r"command: cosh \u{1b}[31m--version"),
        "{out:?}"
    );

    let json = fixture.enable(&["--json", "--dry-run"]);
    assert_success(&json);
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(
        envelope["data"]["notices"][0]["text"],
        "Set title: \u{1b}]2;spoofed-title\u{7}"
    );
    assert_eq!(
        envelope["data"]["notices"][0]["command"],
        "cosh \u{1b}[31m--version"
    );
}

#[test]
fn enable_quiet_suppresses_notices() {
    let fixture = NoticeFixture::new();
    let output = fixture.enable(&["--quiet"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        !out.contains("notices:") && !out.contains("Start a new shell"),
        "--quiet must not print notices: {out}"
    );
}

#[test]
fn enable_json_returns_notices() {
    let fixture = NoticeFixture::new();
    let output = fixture.enable(&["--json"]);
    assert_success(&output);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], true);
    let notices = envelope["data"]["notices"].as_array().expect("array");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["when"], "post_enable");
    assert_eq!(notices[0]["level"], "info");
    assert_eq!(
        notices[0]["text"],
        "Start a new shell to load the extension."
    );
    assert_eq!(notices[0]["command"], "cosh --version");
}

#[test]
fn enable_json_without_notices_is_stable_empty_array() {
    // An adapter with no notices still returns a stable `notices: []`.
    let fixture = NoticeFixture::new();
    let manifest_path = FsLayout::user_with_overrides(
        fixture.home.clone(),
        Some(fixture.data_home.clone()),
        Some(fixture.config_home.clone()),
        Some(fixture.state_home.clone()),
        Some(fixture.cache_home.clone()),
        Some(fixture.runtime_dir.clone()),
    )
    .snapshot_path(COMPONENT);
    std::fs::write(
        &manifest_path,
        r#"[component]
name = "notice-demo"
version = "0.1.0"

[[adapters]]
framework = "cosh"
adapter_type = "extension"
source = "adapters/notice-demo/cosh"
dest = "{datadir}/adapters/{component}/cosh/"
"#,
    )
    .expect("rewrite manifest without notices");

    let output = fixture.enable(&["--json"]);
    assert_success(&output);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(
        envelope["data"]["notices"],
        serde_json::json!([]),
        "notices must be a stable empty array"
    );
}

#[test]
fn enable_dry_run_previews_notices_without_executing() {
    let fixture = NoticeFixture::new();
    let output = fixture.enable(&["--dry-run"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("[dry-run] would enable notice-demo/cosh:"),
        "{out}"
    );
    assert!(out.contains("would show notices:"), "{out}");
    assert!(
        out.contains("[info] Start a new shell to load the extension."),
        "{out}"
    );
    // Preview only: nothing was delivered and no receipt persisted.
    assert!(
        !fixture.extension_dir().exists(),
        "dry-run must not deliver the extension"
    );
}

#[test]
fn enable_dry_run_json_marks_preview_not_executed() {
    let fixture = NoticeFixture::new();
    let output = fixture.enable(&["--json", "--dry-run"]);
    assert_success(&output);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["data"]["dry_run"], true);
    assert!(
        envelope["data"]["claim"].is_null(),
        "dry-run must not carry a claim"
    );
    assert_eq!(envelope["data"]["notices"][0]["when"], "post_enable");
    assert!(
        !fixture.extension_dir().exists(),
        "dry-run must not deliver the extension"
    );
}

#[test]
fn disable_human_shows_post_disable_notices() {
    let fixture = NoticeFixture::new();
    assert_success(&fixture.enable(&[]));
    let output = fixture.disable(&[]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(out.contains("Disabled notice-demo/cosh."), "{out}");
    assert!(
        out.contains("[warning] Extension files were removed from the shell."),
        "{out}"
    );
    // The post_enable notice must not appear on disable.
    assert!(!out.contains("Start a new shell"), "{out}");
}

#[test]
fn disable_json_returns_post_disable_notices() {
    let fixture = NoticeFixture::new();
    assert_success(&fixture.enable(&["--json"]));
    let output = fixture.disable(&["--json"]);
    assert_success(&output);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    let notices = envelope["data"]["notices"].as_array().expect("array");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["when"], "post_disable");
    assert_eq!(notices[0]["level"], "warning");
    assert_eq!(
        notices[0]["text"],
        "Extension files were removed from the shell."
    );
}

#[test]
fn disable_quiet_suppresses_notices() {
    let fixture = NoticeFixture::new();
    assert_success(&fixture.enable(&[]));
    let output = fixture.disable(&["--quiet"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        !out.contains("Extension files were removed"),
        "--quiet must not print notices: {out}"
    );
}

#[test]
fn disable_dry_run_previews_notices_and_keeps_receipt() {
    let fixture = NoticeFixture::new();
    assert_success(&fixture.enable(&[]));
    let output = fixture.disable(&["--dry-run"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("[dry-run] would disable notice-demo/cosh:"),
        "{out}"
    );
    assert!(out.contains("would show notices:"), "{out}");
    assert!(
        out.contains("[warning] Extension files were removed from the shell."),
        "{out}"
    );
    // The receipt survived the preview: a real disable still succeeds.
    let real = fixture.disable(&[]);
    assert_success(&real);
    assert!(stdout(&real).contains("Disabled notice-demo/cosh."));
}
