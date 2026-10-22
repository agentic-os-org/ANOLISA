//! Unified package commands share ownership and offline configuration contracts.

#[path = "packages/output.rs"]
mod output;
#[path = "packages/support.rs"]
mod support;
#[path = "packages/terminal.rs"]
mod terminal;

use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    process::{Command, Output},
};
use support::{success, Fixture};

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
    assert!(diagnostic.contains("--bundle"));
    assert!(diagnostic.contains("required"));
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
    let legacy = Command::new(env!("CARGO_BIN_EXE_aw-package"))
        .args(["install", "--prefix"])
        .output()
        .unwrap();
    assert_eq!(legacy.status.code(), Some(1));
    assert!(legacy.stdout.is_empty());
    assert!(String::from_utf8_lossy(&legacy.stderr).starts_with("aw-package:"));
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
    for (args, usage) in [
        (vec!["install", "--help"], "Usage: aw install"),
        (vec!["config", "init", "--help"], "Usage: aw config init"),
    ] {
        let output = success(
            Command::new(env!("CARGO_BIN_EXE_aw"))
                .args(&args)
                .output()
                .unwrap(),
        );
        assert!(output.contains(usage));
        assert!(output.contains("--prefix <ABS_DIR>"));
        if args[0] == "install" {
            assert!(output.contains("--bundle <ABS_DIR>"));
            assert!(output.contains("no URL or RPM/DEB backend"));
        } else {
            assert!(output.contains("--state-dir <ABS_DIR>"));
        }
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
    assert!(output.contains("Usage: aw config <COMMAND>"));
    assert!(output.contains("init"));
    assert!(output.contains("validate"));
    assert!(!output.contains("aw-package"));
    let output = success(
        Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(["config", "validate", "--help"])
            .output()
            .unwrap(),
    );
    assert!(output.contains("--config <FILE>"));
    assert!(output.contains("--against <BASELINE_FILE>"));
    let output = success(
        Command::new(env!("CARGO_BIN_EXE_aw"))
            .arg("--help")
            .output()
            .unwrap(),
    );
    assert!(output.contains("Install a local Preview bundle"));
    assert!(output.contains("local unpacked Preview bundles only"));
}
