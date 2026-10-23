//! Small byte-limit seams exercise current operations and future complete removal.

use super::*;
use crate::{database::*, plan::Action, transaction::SourceFile, Payload};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool};

fn fixture(root: &std::path::Path) -> (DatabasePaths, ChangeSet, PackagePlan) {
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let paths = DatabasePaths::user(Some(root), None).unwrap();
    let prefix = root.join("prefix");
    fs::create_dir(&prefix).unwrap();
    fs::set_permissions(&prefix, fs::Permissions::from_mode(0o755)).unwrap();
    for name in ["bin", ".aw-packages"] {
        let directory = prefix.join(name);
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let metadata = Payload {
        sha256: "a".repeat(64),
        mode: 0o755,
    };
    let files = BTreeMap::from([("bin/tool".into(), metadata.clone())]);
    let installation_id = "installation".to_owned();
    let after = InstalledDatabase {
        schema_version: 1,
        generation: 1,
        installations: vec![Installation {
            installation_id: installation_id.clone(),
            prefix: prefix.clone(),
            // SAFETY: geteuid only observes the fixture's identity.
            owner_uid: unsafe { libc::geteuid() },
            scope: Scope::User,
            manager: Manager::Raw,
            registration: Registration::Registered,
            packages: vec![InstalledPackage {
                package_id: "example.tool".into(),
                version: "1.0.0".into(),
                os: "linux".into(),
                arch: std::env::consts::ARCH.into(),
                files,
                dependencies: Vec::new(),
                source: SourceEvidence {
                    bundle_sha256: None,
                    source_commit: None,
                },
                provider: None,
            }],
        }],
    };
    let marker = serde_json::to_vec_pretty(
        &RegistrationPointer::new(&paths, installation_id.clone()).unwrap(),
    )
    .unwrap();
    let changes = ChangeSet {
        prefix: prefix.clone(),
        installation_id,
        database_before: None,
        database_after: after,
        verify: BTreeMap::new(),
        writes: BTreeMap::from([(
            "bin/tool".into(),
            SourceFile {
                source: root.join("source"),
                metadata,
            },
        )]),
        removes: BTreeMap::new(),
        legacy: BTreeMap::new(),
        marker_before: None,
        marker_after: Some(marker),
        source_records: BTreeMap::new(),
    };
    let plan = PackagePlan {
        format: 1,
        action: Action::Install,
        prefix,
        scope: Scope::User,
        expected_generation: None,
        installation_id: None,
        register_legacy: false,
        added: Vec::new(),
        removed: Vec::new(),
        files: Vec::new(),
        dependencies: Vec::new(),
        problems: Vec::new(),
        changes: Some(changes.clone()),
        database_root: paths.root.clone(),
    };
    (paths, changes, plan)
}

#[test]
fn encoded_byte_overflow_blocks_execution_before_any_database_or_payload_write() {
    let root = tempfile::tempdir().unwrap();
    let (paths, changes, mut plan) = fixture(root.path());
    let bytes = transaction::capacity::encoded_bytes(&paths, &changes, false).unwrap();
    check_byte_limit(&paths, &mut plan, &changes, false, bytes - 1).unwrap();
    assert!(plan
        .problems
        .iter()
        .any(|problem| problem.code == "transaction_limit"));
    assert!(crate::plan::execute(&paths, &plan, &AtomicBool::new(false)).is_err());
    assert!(!paths.root.exists());
    assert!(!changes.prefix.join("bin/tool").exists());
    assert_eq!(
        fs::read_dir(changes.prefix.join(".aw-packages"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn installation_admission_reserves_the_future_whole_removal_encoded_budget() {
    let root = tempfile::tempdir().unwrap();
    let (paths, changes, plan) = fixture(root.path());
    let current = transaction::capacity::encoded_bytes(&paths, &changes, false).unwrap();
    let mut operation_only = plan.clone();
    check_byte_limit(&paths, &mut operation_only, &changes, false, current).unwrap();
    assert!(operation_only.ready());
    let mut admission = plan;
    check_byte_limit(&paths, &mut admission, &changes, true, current).unwrap();
    assert!(admission
        .problems
        .iter()
        .any(|problem| problem.code == "installation_limit"));
    assert!(crate::plan::execute(&paths, &admission, &AtomicBool::new(false)).is_err());
    assert!(!paths.root.exists());
    assert!(!changes.prefix.join("bin/tool").exists());
}
