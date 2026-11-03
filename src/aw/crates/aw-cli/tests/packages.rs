//! Unified package commands share ownership and offline configuration contracts.

#[path = "packages/confirmation.rs"]
mod confirmation;
#[path = "packages/management.rs"]
mod management;
#[path = "packages/metadata.rs"]
mod metadata;
#[path = "packages/output.rs"]
mod output;
#[path = "packages/scope.rs"]
mod scope;
#[path = "packages/support.rs"]
mod support;
#[path = "packages/terminal.rs"]
mod terminal;
#[path = "packages/validation.rs"]
mod validation;

use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::{Command, Output},
};
use support::{success, Fixture};

fn failure(output: Output) -> String {
    assert_eq!(output.status.code(), Some(1));
    if !output.stdout.is_empty() {
        assert!(String::from_utf8_lossy(&output.stdout).starts_with("Install plan for "));
    }
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.starts_with("aw:"), "{diagnostic}");
    assert!(!diagnostic.contains("panicked"));
    diagnostic
}

#[test]
fn unified_install_init_and_uninstall_preserve_external_and_unowned_files() {
    let f = Fixture::new();
    assert!(success(f.install().output().unwrap()).contains("Installed verified packages"));
    let receipt = f.prefix.join(".local-packages/installation.json");
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
        fs::read_dir(f.prefix.join(".local-packages"))
            .unwrap()
            .count(),
        1
    );
    failure(f.init().output().unwrap());
    assert_eq!(fs::read(&f.config).unwrap(), original_config);

    fs::create_dir(&f.state).unwrap();
    fs::write(f.state.join("retained"), b"runtime data").unwrap();
    fs::write(f.prefix.join("unowned.txt"), b"user data").unwrap();
    success(
        f.command()
            .args(["uninstall", "--yes", "--prefix"])
            .arg(&f.prefix)
            .output()
            .unwrap(),
    );
    assert!(!f.prefix.join("bin/provider").exists());
    let pointer: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(pointer["schema_version"], 1);
    assert_eq!(fs::read(&f.config).unwrap(), original_config);
    assert_eq!(fs::read(f.state.join("retained")).unwrap(), b"runtime data");
    assert_eq!(
        fs::read(f.prefix.join("unowned.txt")).unwrap(),
        b"user data"
    );
}

/// The documented `~/.local` core prefix combined with XDG state
/// (`~/.local/state/aw`) is a supported layout: state is a writable
/// sibling of the installed payloads, not one of them, so `config init`
/// must accept it — with and without a registered Provider in the same
/// prefix. State that would overwrite an installed payload root or the
/// installer's registration metadata is still refused.
#[test]
fn config_init_accepts_xdg_state_beside_payloads_under_the_prefix() {
    for installed in [false, true] {
        let f = Fixture::new();
        let local = f.root.join("local");
        if installed {
            success(
                f.command()
                    .args(["install", "--prefix"])
                    .arg(&local)
                    .arg("--bundle")
                    .arg(&f.bundle)
                    .output()
                    .unwrap(),
            );
            assert!(
                local.join(".local-packages/installation.json").exists(),
                "the registered-prefix case requires the installation"
            );
        }
        // The XDG layout from the issue: core installed at ~/.local, config
        // at ~/.config/aw (outside the prefix), state at ~/.local/state/aw.
        let config = f.root.join(".config/aw/aw.yaml");
        let state = local.join("state/aw");
        // The gate runs under umask 0002 too; create the fixture parents
        // with explicit safe modes so they never inherit group write.
        for parent in [config.parent().unwrap(), state.parent().unwrap()] {
            make_private_dir(parent);
        }
        success(
            f.command()
                .args(["config", "init", "--prefix"])
                .arg(&local)
                .arg("--config")
                .arg(&config)
                .arg("--state-dir")
                .arg(&state)
                .args(["--qoder", "/bin/true"])
                .output()
                .unwrap(),
        );
        assert!(config.is_file());
        assert!(!state.exists(), "configuration must not start a runtime");

        // State targeting an immutable payload root, the installer's
        // reserved registration metadata, or the installation lock is
        // still refused.
        for root in [
            "bin",
            "libexec",
            "share",
            ".local-packages",
            ".aw-packages",
            ".local-install.lock",
        ] {
            let target = f.root.join(format!(".config/aw/aw2-{root}.yaml"));
            failure(
                f.command()
                    .args(["config", "init", "--prefix"])
                    .arg(&local)
                    .arg("--config")
                    .arg(&target)
                    .arg("--state-dir")
                    .arg(local.join(root))
                    .args(["--qoder", "/bin/true"])
                    .output()
                    .unwrap(),
            );
            assert!(!target.exists(), "{root}: no configuration is written");
        }
    }
}

/// Create a directory with 0700 so fixture parents pass the owned-parent
/// gates under any caller umask.
fn make_private_dir(path: &std::path::Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
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
    let output = Command::new(env!("CARGO_BIN_EXE_aw"))
        .args(["install", "--prefix"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("aw:"));
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
fn package_help_uses_the_unified_frontend_name() {
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
    assert!(output.contains("Install a local Provider extension"));
    assert!(output.contains("Provider bundles with package.json"));
}
