//! Native restart regressions; fixtures are supplied by an isolated Linux runner.

use super::*;
use ws_ckpt_common::backend::BackendType;
use ws_ckpt_common::persist::{
    BackendIdentity, BackendPaths, DaemonStateFile, DAEMON_STATE_VERSION,
};

pub(super) fn mount(name: &str) -> std::path::PathBuf {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .expect("explicit isolated Btrfs fixture required")
}

pub(super) fn save_base(state_dir: &Path, mount: &Path) {
    let root = mount.join("ws-ckpt-data");
    let state = DaemonStateFile::new(
        DAEMON_STATE_VERSION,
        BackendIdentity {
            backend_type: BackendType::BtrfsBase,
            selection_method: "auto-detect".into(),
            selected_at: chrono::Utc::now(),
        },
        // Legacy manifests store the data root in mount_path too.
        BackendPaths::BtrfsBase {
            mount_path: root.clone(),
            data_root: root.clone(),
            snapshots_root: root.join("snapshots"),
        },
        Vec::new(),
    );
    persist::save_state(state_dir, &state).unwrap();
}

fn assert_detection_is_other_mount() {
    let detected = std::fs::read_to_string("/proc/mounts").unwrap();
    let first = detected
        .lines()
        .find(|line| {
            let p: Vec<_> = line.split_whitespace().collect();
            p.len() >= 4
                && p[2] == "btrfs"
                && p[0].starts_with("/dev/")
                && !p[0].starts_with("/dev/loop")
                && p[3].split(',').any(|o| o == "rw")
        })
        .expect("writable physical fixture mount must be present");
    let escaped = mount("WS_CKPT_TEST_FIRST_MOUNT")
        .to_string_lossy()
        .replace(' ', "\\040");
    assert_eq!(first.split_whitespace().nth(1).unwrap(), escaped);
}

#[tokio::test]
#[ignore = "requires explicitly provided, isolated writable Btrfs mount fixtures"]
async fn restart_preserves_persisted_root_instead_of_first_detected_mount() {
    assert_detection_is_other_mount();
    let temp = tempfile::tempdir().unwrap();
    let previous = mount("WS_CKPT_TEST_PERSISTED_MOUNT");
    save_base(temp.path(), &previous);
    let state = resolve_state(&DaemonConfig::default(), temp.path())
        .await
        .unwrap();
    assert_eq!(state.backend.data_root(), previous.join("ws-ckpt-data"));
    state.save_manifest().await.unwrap();
    assert_eq!(
        persist::load_state(temp.path())
            .unwrap()
            .unwrap()
            .backend
            .selection_method,
        "persisted"
    );
}

#[tokio::test]
#[ignore = "requires explicitly provided, isolated writable Btrfs mount fixtures"]
async fn missing_persisted_mount_refuses_redetection_and_preserves_manifest() {
    assert_detection_is_other_mount();
    let temp = tempfile::tempdir().unwrap();
    let unmounted = temp.path().join("unmounted");
    std::fs::create_dir(&unmounted).unwrap();
    save_base(temp.path(), &unmounted);
    let path = temp.path().join(ws_ckpt_common::STATE_FILE);
    let before = std::fs::read(&path).unwrap();
    let result = resolve_state(&DaemonConfig::default(), temp.path()).await;
    assert!(
        result.is_err(),
        "must refuse an unmounted persisted root even with another writable Btrfs mount"
    );
    assert!(format!("{:#}", result.err().unwrap()).contains("not mounted"));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[tokio::test]
#[ignore = "requires explicitly provided, isolated writable Btrfs mount fixtures"]
async fn unreadable_manifest_refuses_fresh_detection() {
    assert_detection_is_other_mount();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(ws_ckpt_common::STATE_FILE);
    std::fs::write(&path, b"{invalid manifest").unwrap();
    let result = resolve_state(&DaemonConfig::default(), temp.path()).await;
    assert!(
        result.is_err(),
        "invalid state must not become a new first install"
    );
    assert_eq!(std::fs::read(path).unwrap(), b"{invalid manifest");
}
