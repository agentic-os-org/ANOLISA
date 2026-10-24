//! Operator-selected roots and type transitions on explicitly supplied native fixtures.

use super::native_btrfs_tests::{mount, save_base};
use super::*;
use std::io::Write;
use std::sync::Mutex;
use tracing::instrument::WithSubscriber;
use ws_ckpt_common::backend::BackendType;

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires explicitly provided, isolated writable Btrfs mount fixtures"]
async fn first_install_lists_all_candidates_and_records_selected_root() {
    let temp = tempfile::tempdir().unwrap();
    let output = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let state = resolve_state(&DaemonConfig::default(), temp.path())
        .with_subscriber(subscriber)
        .await
        .unwrap();
    let first = mount("WS_CKPT_TEST_FIRST_MOUNT");
    assert_eq!(state.backend.data_root(), first.join("ws-ckpt-data"));
    let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    for expected in [
        "Multiple writable btrfs",
        "chosen_device",
        "chosen_mount_point",
        "btrfs_mount",
        first.to_str().unwrap(),
        mount("WS_CKPT_TEST_PERSISTED_MOUNT").to_str().unwrap(),
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
    state.save_manifest().await.unwrap();
    let saved = persist::load_state(temp.path()).unwrap().unwrap();
    assert!(
        matches!(saved.paths, persist::BackendPaths::BtrfsBase { data_root, .. } if data_root == first.join("ws-ckpt-data"))
    );
}

#[tokio::test]
#[ignore = "requires explicitly provided, isolated writable Btrfs mount fixtures"]
async fn explicit_pin_can_change_root_with_index_pruning_warning() {
    let temp = tempfile::tempdir().unwrap();
    let previous = mount("WS_CKPT_TEST_FIRST_MOUNT");
    let chosen = mount("WS_CKPT_TEST_PERSISTED_MOUNT");
    save_base(temp.path(), &previous);
    let config = DaemonConfig {
        btrfs_mount: Some(chosen.clone()),
        ..Default::default()
    };
    let output = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let state = resolve_state(&config, temp.path())
        .with_subscriber(subscriber)
        .await
        .unwrap();
    assert_eq!(state.backend.data_root(), chosen.join("ws-ckpt-data"));
    let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    for expected in [
        "Previous snapshots become invisible",
        "prune unpinned",
        "does not recover pruned",
        temp.path().to_str().unwrap(),
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
    state.save_manifest().await.unwrap();
    drop(state);
    let restarted = resolve_state(&DaemonConfig::default(), temp.path())
        .await
        .unwrap();
    assert_eq!(restarted.backend.data_root(), chosen.join("ws-ckpt-data"));
}

#[tokio::test]
#[ignore = "requires explicitly provided, isolated writable Btrfs mount fixtures"]
async fn explicit_type_switch_does_not_adopt_loop_paths() {
    let temp = tempfile::tempdir().unwrap();
    let chosen = mount("WS_CKPT_TEST_PERSISTED_MOUNT");
    let root = temp.path().join("old-loop-root");
    let state_file = persist::DaemonStateFile::new(
        persist::DAEMON_STATE_VERSION,
        persist::BackendIdentity {
            backend_type: BackendType::BtrfsLoop,
            selection_method: "auto-detect".into(),
            selected_at: chrono::Utc::now(),
        },
        persist::BackendPaths::BtrfsLoop {
            mount_path: root.clone(),
            data_root: root.clone(),
            snapshots_root: root.join("snapshots"),
            loop_img: None,
        },
        Vec::new(),
    );
    persist::save_state(temp.path(), &state_file).unwrap();
    let config = DaemonConfig {
        backend_type: "btrfs-base".into(),
        btrfs_mount: Some(chosen.clone()),
        ..Default::default()
    };
    let state = resolve_state(&config, temp.path()).await.unwrap();
    assert_eq!(state.backend.backend_type(), BackendType::BtrfsBase);
    assert_eq!(state.backend.data_root(), chosen.join("ws-ckpt-data"));
    assert!(!root.exists());
}

#[tokio::test]
#[ignore = "requires explicitly provided, isolated writable Btrfs mount fixtures"]
async fn selected_readonly_or_non_btrfs_mount_never_falls_back() {
    let cases = [
        ("WS_CKPT_TEST_READONLY_MOUNT", "read-only"),
        ("WS_CKPT_TEST_NON_BTRFS_MOUNT", "not btrfs"),
    ];
    for (name, diagnostic) in cases {
        let temp = tempfile::tempdir().unwrap();
        let selected = mount(name);
        for configured in [false, true] {
            save_base(temp.path(), &selected);
            let manifest_path = temp.path().join(ws_ckpt_common::STATE_FILE);
            let before = std::fs::read(&manifest_path).unwrap();
            let config = DaemonConfig {
                btrfs_mount: configured.then_some(selected.clone()),
                ..Default::default()
            };
            let result = resolve_state(&config, temp.path()).await;
            assert!(
                result.is_err(),
                "selected {diagnostic} mount must fail even when another usable mount exists"
            );
            assert!(format!("{:#}", result.err().unwrap()).contains(diagnostic));
            assert_eq!(std::fs::read(&manifest_path).unwrap(), before);
        }
    }
}
