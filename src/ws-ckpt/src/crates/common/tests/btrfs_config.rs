use std::path::Path;

use ws_ckpt_common::{load_config_file, save_config_file, FileConfig};

#[test]
fn btrfs_mount_survives_config_round_trip_with_loop_settings() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    std::fs::write(&path, "[backend]\ntype = 'btrfs-base'\n[backend.btrfs-base]\nbtrfs_mount = '/mnt/storage with spaces'\n[backend.btrfs-loop]\nimg_size = 15\nimg_max_percent = 30.0\n").unwrap();
    let config = load_config_file(&path).unwrap();
    assert_eq!(
        config
            .backend
            .btrfs_base
            .as_ref()
            .unwrap()
            .btrfs_mount
            .as_deref(),
        Some(Path::new("/mnt/storage with spaces"))
    );
    assert_eq!(
        config.backend.btrfs_loop.as_ref().unwrap().img_size,
        Some(15)
    );
    save_config_file(&path, &config).unwrap();
    assert_eq!(load_config_file(&path).unwrap(), config);
}

#[test]
fn btrfs_mount_is_optional_for_existing_configurations() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    assert_eq!(load_config_file(&path).unwrap(), FileConfig::default());
    for content in [
        "",
        "[backend]\ntype = 'btrfs-loop'\n",
        "[backend.btrfs-base]\n",
    ] {
        std::fs::write(&path, content).unwrap();
        let config = load_config_file(&path).unwrap();
        assert!(config
            .backend
            .btrfs_base
            .and_then(|b| b.btrfs_mount)
            .is_none());
    }
}

#[test]
fn btrfs_mount_rejects_empty_and_relative_values() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    for value in ["", ".", "../storage", "relative", "~/storage"] {
        std::fs::write(
            &path,
            format!("[backend.btrfs-base]\nbtrfs_mount = '{}'\n", value),
        )
        .unwrap();
        let error = load_config_file(&path).unwrap_err().to_string();
        assert!(
            error.contains("btrfs_mount") && error.contains("absolute"),
            "{error}"
        );
    }
}
