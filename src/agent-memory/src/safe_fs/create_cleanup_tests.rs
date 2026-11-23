use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn exclusive_create_does_not_turn_directory_spelling_into_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = open_root(tmp.path()).unwrap();
    for spelling in ["new/", "new/."] {
        assert!(write_create_new(root.as_fd(), Path::new(spelling), b"body").is_err());
        assert!(!tmp.path().join("new").exists());
    }
}

#[test]
fn cleanup_uses_the_open_parent_after_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let before = tmp.path().join("before");
    let after = tmp.path().join("after");
    std::fs::create_dir(&before).unwrap();
    let parent = File::from(open_root(&before).unwrap());
    let file = File::create(before.join("draft.md")).unwrap();
    std::fs::rename(&before, &after).unwrap();
    std::fs::create_dir(&before).unwrap();
    std::fs::write(before.join("draft.md"), "unrelated").unwrap();
    remove_failed_creation(&parent, Path::new("draft.md"), &file).unwrap();
    assert!(!after.join("draft.md").exists());
    assert_eq!(
        std::fs::read_to_string(before.join("draft.md")).unwrap(),
        "unrelated"
    );
}

#[test]
fn cleanup_keeps_a_replacement_and_tolerates_a_missing_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = File::from(open_root(tmp.path()).unwrap());
    let path = tmp.path().join("draft.md");
    let file = File::create(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    remove_failed_creation(&parent, Path::new("draft.md"), &file).unwrap();
    std::fs::write(&path, "replacement").unwrap();
    remove_failed_creation(&parent, Path::new("draft.md"), &file).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");
    std::fs::remove_file(&path).unwrap();
    symlink("elsewhere", &path).unwrap();
    remove_failed_creation(&parent, Path::new("draft.md"), &file).unwrap();
    assert_eq!(std::fs::read_link(path).unwrap(), Path::new("elsewhere"));
}

#[test]
fn cleanup_reports_permission_failure_when_not_root() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: root bypasses directory write permissions");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let parent = File::from(open_root(tmp.path()).unwrap());
    let path = tmp.path().join("draft.md");
    let file = File::create(&path).unwrap();
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = remove_failed_creation(&parent, Path::new("draft.md"), &file);
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert!(path.exists());
}
