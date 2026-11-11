use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn new_directory_parents_are_synced_before_descendants_are_created() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::fixture(root.path());
    let parents = [root.path().to_path_buf(), root.path().join("cosh")];
    let children = [root.path().join("cosh"), root.path().join("cosh/login")];
    let mut synced = 0;
    let directory = store
        .directory_from(
            File::open(root.path()).unwrap(),
            Path::new("cosh/login"),
            true,
            |parent| {
                let expected = std::fs::metadata(&parents[synced]).unwrap();
                let actual = parent.metadata().unwrap();
                assert_eq!(
                    (actual.dev(), actual.ino()),
                    (expected.dev(), expected.ino())
                );
                assert!(children[synced].is_dir());
                if synced == 0 {
                    assert!(!children[1].exists());
                }
                synced += 1;
                parent.sync_all()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(synced, 2);
    let expected = std::fs::metadata(&children[1]).unwrap();
    let actual = directory.metadata().unwrap();
    assert_eq!(
        (actual.dev(), actual.ino()),
        (expected.dev(), expected.ino())
    );
}

#[test]
fn parent_directory_sync_failure_stops_before_descendant_creation() {
    for fail_at in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::fixture(root.path());
        let mut synced = 0;
        let result = store.directory_from(
            File::open(root.path()).unwrap(),
            Path::new("cosh/login/records"),
            true,
            |parent| {
                synced += 1;
                if synced == fail_at {
                    Err(std::io::Error::from_raw_os_error(nix::libc::EIO))
                } else {
                    parent.sync_all()
                }
            },
        );
        assert!(
            result.is_err(),
            "parent fsync failure must stop directory creation"
        );
        assert_eq!(synced, fail_at);
        assert!(root.path().join("cosh").is_dir());
        assert_eq!(root.path().join("cosh/login").exists(), fail_at == 2);
        assert!(!root.path().join("cosh/login/records").exists());
    }
}

#[test]
fn retry_syncs_existing_parent_before_creating_descendants() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::fixture(root.path());
    let first = store.directory_from(
        File::open(root.path()).unwrap(),
        Path::new("cosh/login"),
        true,
        |_| Err(std::io::Error::from_raw_os_error(nix::libc::EIO)),
    );
    assert!(first.is_err());
    assert!(root.path().join("cosh").is_dir());
    assert!(!root.path().join("cosh/login").exists());
    let mut synced = 0;
    let retry = store.directory_from(
        File::open(root.path()).unwrap(),
        Path::new("cosh/login"),
        true,
        |_| {
            synced += 1;
            Err(std::io::Error::from_raw_os_error(nix::libc::EIO))
        },
    );
    assert!(retry.is_err());
    assert_eq!(synced, 1);
    assert!(
        !root.path().join("cosh/login").exists(),
        "retry must sync the existing cosh entry before creating login"
    );
}
