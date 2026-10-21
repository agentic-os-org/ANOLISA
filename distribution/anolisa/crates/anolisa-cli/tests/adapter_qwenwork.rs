//! QwenWork lifecycle through the public CLI, isolated from the real desktop.
#![cfg(target_os = "macos")]

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Output;

use anolisa_core::domain::ProviderBinding;
use anolisa_core::state::ObjectKind;
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;
use serde_json::Value;
use sha2::{Digest, Sha256};

mod common;

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    app: PathBuf,
    host: PathBuf,
    bin: PathBuf,
    layout: FsLayout,
    source: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let layout = FsLayout::user(home.clone());
        let source = layout.datadir.join("adapters/tokenless/qwenwork");
        let app = home.join("Applications/QwenWorkCN.app");
        let host = home.join(".qwenworkcn");
        let bin = home.join(".local/bin");
        let f = Self {
            _tmp: tmp,
            home,
            app,
            host,
            bin,
            layout,
            source,
        };
        f.write_source(
            ".qoder-plugin/plugin.json",
            r#"{"name":"tokenless","version":"0.1.0"}"#,
        );
        for name in ["hooks/hooks.json", "hooks/shared/tool_categories.json"] {
            f.write_source(name, "{}");
        }
        for name in [
            "hooks/run-hook.sh",
            "hooks/shared/rewrite_hook.py",
            "hooks/shared/compress_response_hook.py",
            "hooks/shared/hook_utils.py",
        ] {
            f.write_source(name, "# fixture\n");
        }
        f.write_source(
            "scripts/uninstall.sh",
            "# legacy script must not be copied\n",
        );
        f.write_source(".tokenless-qwenwork-owned.json", "{}");
        std::fs::create_dir_all(&f.bin).unwrap();
        for name in ["bash", "tokenless", "rtk"] {
            f.program(name, "#!/bin/sh\nexit 0\n");
        }
        f.program("python3", "#!/bin/sh\nprintf '3.9\\n'\nexit 0\n");
        f.app_version("1.2.5", "cn.qwenwork.desktop.mac");
        let manifest = f.layout.snapshot_path("tokenless");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(
            manifest,
            include_str!("../../../../../src/tokenless/.anolisa/component.toml.in")
                .replace("@VERSION@", "0.1.0"),
        )
        .unwrap();
        f.refresh_inventory();
        f
    }

    fn write_source(&self, relative: &str, bytes: &str) {
        let path = self.source.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn program(&self, name: &str, content: &str) {
        let path = self.bin.join(name);
        std::fs::write(&path, content).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn app_version(&self, version: &str, identity: &str) {
        std::fs::create_dir_all(self.app.join("Contents")).unwrap();
        std::fs::write(self.app.join("Contents/Info.plist"), format!(r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{identity}</string><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"#)).unwrap();
    }

    fn refresh_inventory(&self) {
        fn collect(root: &Path, files: &mut Vec<toml::Value>) {
            for entry in std::fs::read_dir(root).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    collect(&path, files);
                } else {
                    files.push(toml::Value::Table(toml::map::Map::from_iter([
                        (
                            "path".to_string(),
                            toml::Value::String(path.to_string_lossy().into_owned()),
                        ),
                        (
                            "owner".to_string(),
                            toml::Value::String("anolisa".to_string()),
                        ),
                        ("kind".to_string(), toml::Value::String("file".to_string())),
                        (
                            "sha256".to_string(),
                            toml::Value::String(format!(
                                "{:x}",
                                Sha256::digest(std::fs::read(path).unwrap())
                            )),
                        ),
                    ])));
                }
            }
        }
        let path = self.layout.state_dir.join("installed.toml");
        let mut state: toml::Value = if path.exists() {
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
        } else {
            toml::from_str(&format!(
                r#"schema_version = 2
updated_at = "2026-10-14T00:00:00Z"
install_mode = "user"
prefix = "{}"
anolisa_version = "0.3.17"
[[objects]]
kind = "component"
name = "tokenless"
version = "0.1.0"
status = "installed"
install_backend = "raw"
ownership = "raw_managed"
installed_at = "2026-10-14T00:00:00Z"
"#,
                self.layout.prefix.display()
            ))
            .unwrap()
        };
        let mut files = Vec::new();
        collect(&self.source, &mut files);
        std::fs::create_dir_all(&self.layout.state_dir).unwrap();
        if !path.exists() {
            state["objects"][0]
                .as_table_mut()
                .unwrap()
                .insert("files".to_string(), toml::Value::Array(files.clone()));
            std::fs::write(&path, toml::to_string(&state).unwrap()).unwrap();
        }
        let mut installed =
            StateStore::load(&path, anolisa_platform::privilege::effective_uid()).unwrap();
        let installation = installed
            .find_mut(ObjectKind::Component, "tokenless")
            .unwrap();
        let ProviderBinding::Owned { artifact } = &mut installation.binding else {
            panic!("expected owned fixture");
        };
        artifact.files = files
            .into_iter()
            .map(|file| file.try_into().unwrap())
            .collect();
        installed.save(&path).unwrap();
    }

    fn run(&self, flags: &[&str], command: &[&str]) -> Output {
        self.run_home(flags, command, &self.host)
    }

    fn run_home(&self, flags: &[&str], command: &[&str], host: &Path) -> Output {
        let mut args = vec!["--json", "--install-mode", "user"];
        args.extend_from_slice(flags);
        args.push("adapter");
        args.extend_from_slice(command);
        common::run_with_path_env(
            &args,
            &[
                ("HOME", &self.home),
                ("XDG_DATA_HOME", &self.home.join(".local/share")),
                ("XDG_CONFIG_HOME", &self.home.join(".config")),
                ("XDG_STATE_HOME", &self.home.join(".local/state")),
                ("XDG_CACHE_HOME", &self.home.join(".cache")),
                ("XDG_RUNTIME_DIR", &self.home.join(".runtime")),
                ("TOKENLESS_QWENWORK_APP", &self.app),
                ("TOKENLESS_QWENWORK_HOME", host),
                ("PATH", &self.bin),
            ],
        )
    }

    fn ok(&self, flags: &[&str], command: &[&str]) -> Value {
        let output = self.run(flags, command);
        json_success(output)
    }

    fn target(&self) -> PathBuf {
        self.host.join("plugins-custom/tokenless")
    }
}

fn json_success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(output: Output, expected: &str) {
    assert!(!output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains(expected), "{text}");
}

#[test]
fn lifecycle_dry_run_upgrade_and_source_free_disable() {
    let f = Fixture::new();
    let before = std::fs::read(f.layout.state_dir.join("installed.toml")).unwrap();
    let scan = f.ok(&[], &["scan"]);
    assert!(scan.to_string().contains("qwenwork"));
    let plan = f.ok(&["--dry-run"], &["enable", "tokenless", "qwenwork"]);
    assert!(plan.to_string().contains("activation"));
    assert!(!f.host.exists());
    assert_eq!(
        before,
        std::fs::read(f.layout.state_dir.join("installed.toml")).unwrap()
    );
    let enabled = f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    assert!(enabled.to_string().contains("Restart QwenWorkCN"));
    assert!(f.target().join("hooks/shared/hook_utils.py").exists());
    assert!(!f.target().join("scripts").exists());
    assert!(!f.target().join(".tokenless-qwenwork-owned.json").exists());
    assert!(!f.host.join("settings.json").exists());
    f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    let status = f.ok(&[], &["status", "tokenless"]);
    let receipt = &status["data"]["receipts"][0];
    assert_eq!(receipt["summary"], "unknown", "{status}");
    for kind in ["activation_enabled", "plugin_resources_loaded"] {
        assert!(
            receipt["conditions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["kind"] == kind && c["status"] == "unknown"),
            "{status}"
        );
    }
    f.write_source("obsolete.txt", "old");
    f.refresh_inventory();
    f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    std::fs::remove_file(f.source.join("obsolete.txt")).unwrap();
    f.write_source("hooks/shared/hook_utils.py", "# upgraded\n");
    f.refresh_inventory();
    f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    assert!(!f.target().join("obsolete.txt").exists());
    assert_eq!(
        std::fs::read_to_string(f.target().join("hooks/shared/hook_utils.py")).unwrap(),
        "# upgraded\n"
    );
    f.ok(&["--dry-run"], &["disable", "tokenless", "qwenwork"]);
    assert!(f.target().exists());
    std::fs::remove_dir_all(&f.app).unwrap();
    std::fs::remove_dir_all(&f.source).unwrap();
    std::fs::remove_dir_all(&f.bin).unwrap();
    f.ok(&[], &["disable", "tokenless", "qwenwork"]);
    assert!(!f.target().exists());
    f.ok(&[], &["disable", "tokenless", "qwenwork"]);
}

#[test]
fn foreign_install_and_modified_files_are_preserved() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.target()).unwrap();
    let foreign_receipt = f.target().join(".tokenless-qwenwork-owned.json");
    std::fs::write(&foreign_receipt, "{}").unwrap();
    failure(
        f.run(&[], &["enable", "tokenless", "qwenwork"]),
        "content ANOLISA does not own",
    );
    assert_eq!(std::fs::read_to_string(foreign_receipt).unwrap(), "{}");
    assert!(!f.target().join(".qoder-plugin/plugin.json").exists());
    assert!(
        f.ok(&[], &["status", "tokenless"])["data"]["receipts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    std::fs::remove_dir_all(f.target()).unwrap();
    f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    let edited = f.target().join("hooks/shared/hook_utils.py");
    std::fs::write(&edited, "# user edit").unwrap();
    failure(f.run(&[], &["enable", "tokenless", "qwenwork"]), "modified");
    assert_eq!(
        f.ok(&[], &["status", "tokenless"])["data"]["receipts"][0]["summary"],
        "degraded"
    );
    let output = f.run(&[], &["disable", "tokenless", "qwenwork"]);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("modified"),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(&edited).unwrap(), "# user edit");
    assert_eq!(
        f.ok(&[], &["status", "tokenless"])["data"]["receipts"][0]["summary"],
        "cleanup_failed"
    );
    std::fs::remove_file(edited).unwrap();
    f.ok(&[], &["disable", "tokenless", "qwenwork"]);
    assert!(!f.target().exists());
}

#[test]
fn unowned_files_and_conflicting_upgrade_are_preserved() {
    let f = Fixture::new();
    f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    std::fs::write(f.target().join("user.txt"), "user").unwrap();
    f.write_source("user.txt", "package");
    f.refresh_inventory();
    failure(f.run(&[], &["enable", "tokenless", "qwenwork"]), "unowned");
    f.ok(&[], &["disable", "tokenless", "qwenwork"]);
    assert_eq!(
        std::fs::read_to_string(f.target().join("user.txt")).unwrap(),
        "user"
    );
}

#[test]
fn custom_home_changes_and_symlink_escape_are_rejected() {
    let f = Fixture::new();
    f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    let nested = f.home.join("nested/data");
    let output = f.run_home(&[], &["enable", "tokenless", "qwenwork"], &nested);
    assert!(!output.status.success());
    assert!(!nested.exists());
    assert!(f.target().exists());
    f.ok(&[], &["disable", "tokenless", "qwenwork"]);
    std::fs::remove_dir_all(&f.host).unwrap();
    let outside = f.home.join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, &f.host).unwrap();
    failure(f.run(&[], &["enable", "tokenless", "qwenwork"]), "symlink");
    assert!(std::fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn app_identity_runtime_requirements_and_version_warning() {
    let f = Fixture::new();
    f.app_version("1.2.5", "wrong.bundle");
    failure(
        f.run(&[], &["enable", "tokenless", "qwenwork"]),
        "bundle identifier",
    );
    f.app_version("1.2.6", "cn.qwenwork.desktop.mac");
    f.program("python3", "#!/bin/sh\nexit 1\n");
    failure(
        f.run(&[], &["enable", "tokenless", "qwenwork"]),
        "Python 3.9+",
    );
    assert!(!f.host.exists());
    f.program("python3", "#!/bin/sh\nexit 0\n");
    let result = f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    assert!(result.to_string().contains("compatibility is unverified"));
}

#[test]
fn missing_runtime_resource_and_hardlinked_copy_fail_safely() {
    let f = Fixture::new();
    std::fs::remove_file(f.source.join("hooks/shared/hook_utils.py")).unwrap();
    assert!(
        !f.run(&[], &["enable", "tokenless", "qwenwork"])
            .status
            .success()
    );
    assert!(!f.host.exists());
    f.write_source("hooks/shared/hook_utils.py", "# fixture\n");
    f.ok(&[], &["enable", "tokenless", "qwenwork"]);
    let path = f.target().join("hooks/shared/hook_utils.py");
    let alias = f.home.join("hardlink");
    std::fs::hard_link(&path, &alias).unwrap();
    failure(
        f.run(&[], &["enable", "tokenless", "qwenwork"]),
        "hard-linked",
    );
    f.run(&[], &["disable", "tokenless", "qwenwork"]);
    assert!(path.exists() && alias.exists());
}
