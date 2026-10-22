//! Unified package commands share ownership and offline configuration contracts.

use aw_package::{Component, Manifest, Payload};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    bundle: PathBuf,
    prefix: PathBuf,
    config: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = directory.path().canonicalize().unwrap();
        let bundle = root.join("bundle");
        let mut files = BTreeMap::new();
        for name in ["bin/aw", "bin/aw-package"] {
            let path = bundle.join("payload").join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::copy("/bin/true", &path).unwrap();
            files.insert(
                name.into(),
                Payload {
                    sha256: aw_package::digest(&path).unwrap(),
                    mode: 0o755,
                },
            );
        }
        fs::write(
            bundle.join("manifest.json"),
            serde_json::to_vec(&Manifest {
                format: 1,
                components: vec![Component {
                    format: 1,
                    component: "aw-core".into(),
                    version: "0.1.0-preview.1".into(),
                    source_commit: "a".repeat(40),
                    os: "linux".into(),
                    arch: std::env::consts::ARCH.into(),
                    provider_protocol: "aw-provider/v1alpha1".into(),
                    requires: BTreeMap::new(),
                    files,
                }],
            })
            .unwrap(),
        )
        .unwrap();
        Self {
            prefix: root.join("prefix"),
            config: root.join("aw.yaml"),
            state: root.join("state"),
            _directory: directory,
            root,
            bundle,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aw"));
        command.current_dir(&self.root);
        command
    }

    fn install(&self) -> Command {
        let mut command = self.command();
        command
            .args(["install", "--prefix"])
            .arg(&self.prefix)
            .arg("--bundle")
            .arg(&self.bundle);
        command
    }

    fn init(&self) -> Command {
        let mut command = self.command();
        command
            .args(["config", "init", "--prefix"])
            .arg(&self.prefix)
            .arg("--config")
            .arg(&self.config)
            .arg("--state-dir")
            .arg(&self.state)
            .args(["--qoder", "/bin/true"]);
        command
    }

    fn edit(&self, action: &str, arguments: &[&str]) -> Command {
        let mut command = self.command();
        command
            .args(["config", action, "--config"])
            .arg(&self.config)
            .args(arguments);
        command
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).unwrap()
}

fn failure(output: Output) -> String {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.starts_with("aw:"), "{diagnostic}");
    assert!(!diagnostic.contains("panicked"));
    diagnostic
}

#[test]
fn unified_install_init_and_uninstall_preserve_external_and_unowned_files() {
    let f = Fixture::new();
    assert!(success(f.install().output().unwrap()).contains("Installed verified AW components"));
    let receipt = f.prefix.join(".aw-packages/aw-core.json");
    let original_receipt = fs::read(&receipt).unwrap();
    failure(f.install().output().unwrap());
    assert_eq!(fs::read(&receipt).unwrap(), original_receipt);

    success(f.init().output().unwrap());
    let original_config = fs::read(&f.config).unwrap();
    let value: Value = serde_yaml_ng::from_slice(&original_config).unwrap();
    assert_eq!(value["spec"]["providers"], json!({}));
    assert_eq!(value["spec"]["events"], json!({}));
    assert!(!f.state.exists(), "configuration must not start a runtime");
    assert_eq!(
        fs::read_dir(f.prefix.join(".aw-packages")).unwrap().count(),
        1
    );
    failure(f.init().output().unwrap());
    assert_eq!(fs::read(&f.config).unwrap(), original_config);

    fs::create_dir(&f.state).unwrap();
    fs::write(f.state.join("retained"), b"runtime data").unwrap();
    fs::write(f.prefix.join("unowned.txt"), b"user data").unwrap();
    success(
        f.command()
            .args(["uninstall", "--prefix"])
            .arg(&f.prefix)
            .output()
            .unwrap(),
    );
    assert!(!f.prefix.join("bin/aw").exists());
    assert!(!receipt.exists());
    assert_eq!(fs::read(&f.config).unwrap(), original_config);
    assert_eq!(fs::read(f.state.join("retained")).unwrap(), b"runtime data");
    assert_eq!(
        fs::read(f.prefix.join("unowned.txt")).unwrap(),
        b"user data"
    );
}

#[test]
fn only_legacy_package_entrypoint_infers_an_adjacent_bundle() {
    let f = Fixture::new();
    let unified = f.bundle.join("aw");
    let legacy = f.bundle.join("aw-package");
    fs::copy(env!("CARGO_BIN_EXE_aw"), &unified).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_aw-package"), &legacy).unwrap();

    let diagnostic = failure(
        Command::new(&unified)
            .args(["install", "--prefix"])
            .arg(&f.prefix)
            .output()
            .unwrap(),
    );
    assert!(diagnostic.contains("missing --bundle"));
    assert!(!f.prefix.exists());
    success(
        Command::new(&legacy)
            .args(["install", "--prefix"])
            .arg(&f.prefix)
            .output()
            .unwrap(),
    );
    assert!(f.prefix.join(".aw-packages/aw-core.json").is_file());
}

#[test]
fn unified_configuration_edits_are_offline_and_guard_provider_references() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    fs::write(
        f.root.join("provider.json"),
        serde_json::to_vec(&json!({
            "protocol":"aw-provider/v1alpha1",
            "transport":{"type":"stdio","location":"agent","argv":["/unexecuted/provider"]},
            "timeout_ms":1000,"max_output_bytes":4096,"config":{}
        }))
        .unwrap(),
    )
    .unwrap();
    let add = ["--name", "external", "--definition", "provider.json"];
    success(f.edit("add-provider", &add).output().unwrap());
    let bytes = fs::read(&f.config).unwrap();
    let inode = fs::metadata(&f.config).unwrap().ino();
    assert_eq!(
        success(f.edit("add-provider", &add).output().unwrap()).trim(),
        "Configuration unchanged"
    );
    assert_eq!(fs::read(&f.config).unwrap(), bytes);
    assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
    let shown: Value = serde_json::from_str(&success(
        f.edit("show", &["--provider", "external"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert_eq!(shown["transport"]["argv"], json!(["/unexecuted/provider"]));

    fs::write(
        f.root.join("event.json"),
        serde_json::to_vec(&json!({"enabled":false,"steps":[{
            "id":"first","provider":"external","operation":"observe",
            "effects":["observe"],"on_error":"report"
        }]}))
        .unwrap(),
    )
    .unwrap();
    success(
        f.edit(
            "add-event",
            &["--event", "tool.before", "--definition", "event.json"],
        )
        .output()
        .unwrap(),
    );
    let referenced = fs::read(&f.config).unwrap();
    failure(
        f.edit("remove-provider", &["--name", "external"])
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read(&f.config).unwrap(), referenced);
    success(
        f.edit("remove-hook", &["--event", "tool.before", "--id", "first"])
            .output()
            .unwrap(),
    );
    success(
        f.edit("remove-provider", &["--name", "external"])
            .output()
            .unwrap(),
    );
    assert!(!f.state.exists());
}

#[test]
fn package_and_native_install_options_cannot_mix_or_write_on_invalid_arguments() {
    let f = Fixture::new();
    for extra in [
        vec!["--config", "missing.yaml", "--agent", "qoder"],
        vec!["--prefix", "/duplicate"],
        vec!["--native-profile", "unexpected"],
        vec!["--unknown", "option"],
    ] {
        failure(f.install().args(extra).output().unwrap());
        assert!(!f.prefix.exists());
        assert!(!f.config.exists());
    }
    failure(
        f.command()
            .args(["install", "https://example.invalid/manT.tar.gz"])
            .output()
            .unwrap(),
    );
    assert!(!f.prefix.exists());
    failure(
        f.command()
            .args(["config", "init", "--prefix"])
            .output()
            .unwrap(),
    );
    assert!(!f.config.exists());
}

#[test]
fn unified_config_validate_keeps_relative_paths_and_policy_update_validation() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    let original = fs::read(&f.config).unwrap();
    let inode = fs::metadata(&f.config).unwrap().ino();
    for extra in [vec![], vec!["--against", "aw.yaml"]] {
        success(
            f.command()
                .args(["config", "validate", "--config", "aw.yaml"])
                .args(extra)
                .output()
                .unwrap(),
        );
    }
    assert_eq!(fs::read(&f.config).unwrap(), original);
    assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
}

#[test]
fn package_help_uses_the_selected_frontend_name() {
    for args in [vec!["install", "--help"], vec!["config", "init", "--help"]] {
        let output = success(
            Command::new(env!("CARGO_BIN_EXE_aw"))
                .args(args)
                .output()
                .unwrap(),
        );
        assert!(output.contains("aw install --prefix ABS_DIR --bundle ABS_DIR"));
        assert!(!output.contains("[--bundle"));
        assert!(output.contains("aw config init --prefix"));
        assert!(!output.contains("aw-package"));
    }
    let output = success(
        Command::new(env!("CARGO_BIN_EXE_aw-package"))
            .arg("--help")
            .output()
            .unwrap(),
    );
    assert!(output.contains("aw-package configure --prefix"));
    assert!(output.contains("aw-package install --prefix ABS_DIR [--bundle ABS_DIR]"));
    assert!(!output.contains("aw config init"));
    assert!(!output.contains("--against"));
    let output = success(
        Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(["config", "--help"])
            .output()
            .unwrap(),
    );
    assert!(output.contains("aw config init --prefix"));
    assert!(output.contains("aw config validate --config FILE [--against BASELINE_FILE]"));
    assert!(!output.contains("aw-package"));
    let output = success(
        Command::new(env!("CARGO_BIN_EXE_aw"))
            .arg("--help")
            .output()
            .unwrap(),
    );
    assert!(output.contains("aw install --prefix ABSOLUTE_DIR --bundle ABSOLUTE_DIR"));
    assert!(!output.contains("[--bundle"));
}
