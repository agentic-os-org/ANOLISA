//! Exercise OpenCode registration through the public CLI in an isolated user layout.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use anolisa_platform::fs_layout::FsLayout;
use serde_json::Value;
use sha2::{Digest, Sha256};

mod common;

#[test]
fn opencode_cli_lifecycle_and_dry_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    let data = root.join("data");
    let config = root.join("config");
    let state = root.join("state");
    let cache = root.join("cache");
    let runtime = root.join("runtime");
    let layout = FsLayout::user_with_overrides(
        home.clone(),
        Some(data.clone()),
        Some(config.clone()),
        Some(state.clone()),
        Some(cache.clone()),
        Some(runtime.clone()),
    );
    let plugin = layout.datadir.join("adapters/tokenless/opencode/plugin.js");
    let source = "export const Plugin = async () => ({});\n";
    std::fs::create_dir_all(plugin.parent().unwrap()).unwrap();
    std::fs::write(&plugin, source).unwrap();
    let manifest = layout.snapshot_path("tokenless");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(
        manifest,
        include_str!("../../../../../src/tokenless/.anolisa/component.toml.in")
            .replace("@VERSION@", "0.1.0"),
    )
    .unwrap();
    std::fs::create_dir_all(&layout.state_dir).unwrap();
    std::fs::write(
        layout.state_dir.join("installed.toml"),
        format!(
            r#"schema_version = 2
updated_at = "2026-09-18T00:00:00Z"
install_mode = "user"
prefix = "{}"
anolisa_version = "0.3.12"
[[objects]]
kind = "component"
name = "tokenless"
version = "0.1.0"
status = "installed"
install_backend = "raw"
ownership = "raw_managed"
installed_at = "2026-09-18T00:00:00Z"
[[objects.files]]
path = "{}"
owner = "anolisa"
kind = "file"
sha256 = "{:x}"
"#,
            layout.prefix.display(),
            plugin.display(),
            Sha256::digest(source)
        ),
    )
    .unwrap();
    let opencode = root.join("opencode");
    std::fs::write(&opencode, "#!/bin/sh\nexit 99\n").unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
    let host_config = config.join("opencode");
    let run = |flags: &[&str], command: &[&str]| -> Value {
        let mut args = vec!["--json", "--install-mode", "user"];
        args.extend_from_slice(flags);
        args.push("adapter");
        args.extend_from_slice(command);
        let output = common::run_with_path_env(
            &args,
            &[
                ("HOME", &home),
                ("XDG_DATA_HOME", &data),
                ("XDG_CONFIG_HOME", &config),
                ("XDG_STATE_HOME", &state),
                ("XDG_CACHE_HOME", &cache),
                ("XDG_RUNTIME_DIR", &runtime),
                ("OPENCODE_CONFIG_DIR", &host_config),
                ("OPENCODE_BIN", &opencode),
            ],
        );
        assert!(
            output.status.success(),
            "stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let scan = run(&[], &["scan"]);
    assert!(scan.to_string().contains("opencode"));
    let link = host_config.join("plugins/tokenless.js");
    run(&["--dry-run"], &["enable", "tokenless", "opencode"]);
    assert!(!host_config.exists());
    let enabled = run(&[], &["enable", "tokenless", "opencode"]);
    assert!(enabled.to_string().contains("Restart OpenCode"));
    assert_eq!(std::fs::read_link(&link).unwrap(), plugin);
    let status = run(&[], &["status", "tokenless"]);
    let receipt = &status["data"]["receipts"][0];
    assert_eq!(receipt["summary"], "unknown", "{status}");
    assert!(
        receipt["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |condition| condition["kind"] == "symlink_present" && condition["status"] == "true"
            )
    );
    run(&["--dry-run"], &["disable", "tokenless", "opencode"]);
    assert!(link.is_symlink());
    let disabled = run(&[], &["disable", "tokenless", "opencode"]);
    assert!(
        disabled["data"]["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice["when"] == "post_disable"
                && notice["level"] == "warning"
                && notice["text"] == "Restart OpenCode to unload the Tokenless plugin."),
        "{disabled}"
    );
    assert!(!link.is_symlink());
    assert!(
        run(&[], &["status", "tokenless"])["data"]["receipts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
