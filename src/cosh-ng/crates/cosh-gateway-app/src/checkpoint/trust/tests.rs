use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

use super::*;

#[test]
fn only_transient_checkpoint_admission_failures_are_unavailable() {
    assert!(CheckpointAdmissionError::Socket.is_unavailable());
    assert!(CheckpointAdmissionError::Identity.is_unavailable());
    for failure in [
        CheckpointAdmissionError::Profile,
        CheckpointAdmissionError::SocketTrust,
        CheckpointAdmissionError::Workspace,
        CheckpointAdmissionError::Audit,
    ] {
        assert!(!failure.is_unavailable(), "{failure}");
    }
}

fn owner_uid() -> u32 {
    nix::unistd::Uid::effective().as_raw()
}

fn private_directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(&path).unwrap();
    path
}

#[test]
fn a_new_audit_log_has_a_header_and_can_be_reopened() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = private_directory(root.path(), "audit");
    let path = directory.join("security.jsonl");

    let first = open_audit_file(&path, owner_uid()).unwrap();
    drop(first);

    assert_eq!(fs::read(&path).unwrap(), AUDIT_HEADER);
    let reopened = open_audit_file(&path, owner_uid()).unwrap();
    drop(reopened);
}

#[test]
fn a_concurrently_locked_audit_log_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = private_directory(root.path(), "audit");
    let path = directory.join("security.jsonl");
    let first = open_audit_file(&path, owner_uid()).unwrap();

    assert!(matches!(
        open_audit_file(&path, owner_uid()),
        Err(CheckpointAdmissionError::Audit)
    ));

    drop(first);
}

#[test]
fn an_arbitrary_existing_private_file_is_not_an_audit_log() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = private_directory(root.path(), "audit");
    let path = directory.join("private-data");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(b"not an audit log\n").unwrap();
    file.sync_all().unwrap();
    drop(file);

    assert!(matches!(
        open_audit_file(&path, owner_uid()),
        Err(CheckpointAdmissionError::Audit)
    ));
    assert_eq!(fs::read(&path).unwrap(), b"not an audit log\n");
}

#[test]
fn a_non_sticky_writable_ancestor_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let writable = root.path().join("writable");
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o777).create(&writable).unwrap();
    fs::set_permissions(&writable, fs::Permissions::from_mode(0o777)).unwrap();
    let directory = private_directory(&writable, "audit");
    let path = directory.join("security.jsonl");

    assert!(matches!(
        open_audit_file(&path, owner_uid()),
        Err(CheckpointAdmissionError::Audit)
    ));
    assert!(!path.exists());
}
