//! Encoded bounds use the journal serializer without creating deep payload directories.

use super::*;
use crate::{database::*, transaction::SourceFile, Payload};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

fn changes(root: &Path) -> (DatabasePaths, ChangeSet) {
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let paths = DatabasePaths::user(Some(root), None).unwrap();
    let prefix = root.join("prefix");
    let files = BTreeMap::from([(
        "bin/tool".into(),
        Payload {
            sha256: "a".repeat(64),
            mode: 0o755,
        },
    )]);
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
                files: files.clone(),
                dependencies: Vec::new(),
                source: SourceEvidence {
                    bundle_sha256: None,
                    source_commit: None,
                },
                provider: None,
            }],
        }],
    };
    let marker_after = Some(
        serde_json::to_vec_pretty(
            &RegistrationPointer::new(&paths, installation_id.clone()).unwrap(),
        )
        .unwrap(),
    );
    let changes = ChangeSet {
        prefix,
        installation_id,
        database_before: None,
        database_after: after,
        verify: BTreeMap::new(),
        writes: files
            .into_iter()
            .map(|(name, metadata)| {
                (
                    name,
                    SourceFile {
                        source: root.join("source"),
                        metadata,
                    },
                )
            })
            .collect(),
        removes: BTreeMap::new(),
        legacy: BTreeMap::new(),
        marker_before: None,
        marker_after,
        source_records: BTreeMap::new(),
    };
    (paths, changes)
}

#[test]
fn existing_parents_keep_the_same_conservative_byte_reservation() {
    let root = tempfile::tempdir().unwrap();
    let (paths, changes) = changes(root.path());
    let before = encoded_bytes(&paths, &changes, false).unwrap();
    fs::create_dir(&changes.prefix).unwrap();
    fs::set_permissions(&changes.prefix, fs::Permissions::from_mode(0o755)).unwrap();
    for name in ["bin", ".aw-packages"] {
        let directory = changes.prefix.join(name);
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(directories(&paths, &changes).unwrap(), 0);
    assert_eq!(encoded_bytes(&paths, &changes, false).unwrap(), before);
}

#[test]
fn legacy_final_locator_removal_reserves_its_post_gate_before_identity() {
    let root = tempfile::tempdir().unwrap();
    let (paths, mut changes) = changes(root.path());
    changes.legacy.insert("aw-core.json".into(), b"{}".to_vec());
    changes.marker_before = Some(b"{}".to_vec());
    changes.marker_after = None;
    let entry = bounded_journal(&paths, &changes, false).unwrap();
    let locator_actions = entry
        .actions
        .iter()
        .filter(|action| action.path == ".aw-packages/aw-core.json")
        .collect::<Vec<_>>();
    assert_eq!(locator_actions.len(), 2);
    assert!(locator_actions[0].after.is_some());
    assert!(locator_actions[1].before.is_some());
    assert!(locator_actions[1].after.is_none());
}

#[test]
fn future_removal_reserves_maximum_snapshots_despite_current_database_growth() {
    let root = tempfile::tempdir().unwrap();
    let (paths, mut changes) = changes(root.path());
    let before = encoded_bytes(&paths, &changes, true).unwrap();
    let mut database = serde_json::to_vec(&InstalledDatabase {
        schema_version: 1,
        generation: 0,
        installations: Vec::new(),
    })
    .unwrap();
    database.extend(std::iter::repeat_n(b'\n', 4096));
    changes.database_before = Some(database);
    assert_eq!(encoded_bytes(&paths, &changes, true).unwrap(), before);
    assert!(before >= 4 * MAX_DATABASE_BYTES);
}

#[test]
fn counted_journal_matches_real_serde_and_bounds_shorter_runtime_identities() {
    let root = tempfile::tempdir().unwrap();
    let (paths, mut changes) = changes(root.path());
    let mut before = serde_json::to_vec(&InstalledDatabase {
        schema_version: 1,
        generation: 0,
        installations: Vec::new(),
    })
    .unwrap();
    before.extend_from_slice(b"\n\t");
    changes.database_before = Some(before);
    let mut entry = bounded_journal(&paths, &changes, false).unwrap();
    let bound = encoded_bytes(&paths, &changes, false).unwrap();
    assert_eq!(bound, serde_json::to_vec(&entry).unwrap().len() as u64);
    entry.transaction_id = "txn-1-2-3".into();
    for action in &mut entry.actions {
        for value in action.before.iter_mut().chain(action.after.iter_mut()) {
            value.dev = 1;
            value.ino = 2;
            value.mode = 0o755;
        }
        action.active = true;
    }
    assert!(bound > serde_json::to_vec(&entry).unwrap().len() as u64);
    assert!(!changes.prefix.exists());
    assert!(!paths.root.exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn deep_parent_paths_and_legacy_locator_actions_enter_the_encoded_budget() {
    let root = tempfile::tempdir().unwrap();
    let (paths, mut changes) = changes(root.path());
    let name = format!("share/{}/quoted\"/tab\t/雪/tool", "long".repeat(40));
    let source = changes.writes.remove("bin/tool").unwrap();
    changes.writes.insert(name.clone(), source);
    changes.marker_before = Some(b"{}".to_vec());
    changes.legacy = [
        "aw-core.json",
        "aw-provider-sec-core.json",
        "aw-provider-tokenless.json",
    ]
    .into_iter()
    .map(|name| (name.into(), b"{}".to_vec()))
    .collect();
    let entry = bounded_journal(&paths, &changes, false).unwrap();
    assert_eq!(entry.actions.len(), actions(&paths, &changes).unwrap());
    assert!(entry.directories.contains_key(".aw-packages"));
    let parent = PathBuf::from(name)
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(entry.directories.contains_key(&parent));
    let with_parents = encoded_bytes(&paths, &changes, false).unwrap();
    let mut without_parents = entry;
    without_parents.directories.clear();
    assert!(with_parents > serde_json::to_vec(&without_parents).unwrap().len() as u64);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}
