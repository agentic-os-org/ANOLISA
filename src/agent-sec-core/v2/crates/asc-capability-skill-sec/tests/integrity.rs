use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::PathBuf;
use std::sync::Arc;

use asc_capability_skill_sec::{
    HashDiff, KeyStore, Manifest, SkillIdentity, SkillSecConfig, hash_tree,
};

fn state() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn manifest() -> Manifest {
    Manifest::initial(
        SkillIdentity::new("/srv/skills/alpha").unwrap(),
        BTreeMap::new(),
    )
}

#[test]
fn independent_utf8_canonical_hash_and_ed25519_fixture_matches() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/integrity.json")).unwrap();
    let hex = fixture["pkcs8Hex"].as_str().unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    let (_dir, path) = state();
    let key_file = path.join("signing-key.pk8");
    fs::write(&key_file, bytes).unwrap();
    fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600)).unwrap();
    let key = KeyStore::open(&path).unwrap().load().unwrap();
    let expected: Manifest = serde_json::from_value(fixture["manifest"].clone()).unwrap();
    key.verify_manifest(&expected, &expected.canonical_skill_dir)
        .unwrap();
    let mut actual = expected.clone();
    actual.manifest_hash.clear();
    actual.signature = None;
    key.sign_manifest(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn canonical_identity_is_lexical_and_validated_after_json_decode() {
    for invalid in [
        "/",
        "relative",
        "//srv/skill",
        "/srv//skill",
        "/srv/../skill",
        "/srv/./skill",
        "/srv/skill/",
        "/srv/\0skill",
    ] {
        assert!(SkillIdentity::new(invalid).is_err(), "{invalid:?}");
        assert!(serde_json::from_value::<SkillIdentity>(serde_json::json!(invalid)).is_err());
    }
    let left = SkillIdentity::new("/srv/one/alpha").unwrap();
    let right = SkillIdentity::new("/srv/two/alpha").unwrap();
    assert_eq!(left.name(), right.name());
    assert_ne!(left, right);
    let config = SkillSecConfig {
        state_dir: "/var/lib/agent-sec/skillsec".into(),
        managed_skill_dirs: vec![left],
    };
    config.validate().unwrap();
}

#[test]
fn current_key_authenticates_roundtrip_and_rejects_metadata_tampering() {
    let (_dir, path) = state();
    let key = KeyStore::open(&path).unwrap().initialize().unwrap();
    let mut record = manifest();
    key.sign_manifest(&mut record).unwrap();
    let json = serde_json::to_string(&record).unwrap();
    let decoded: Manifest = serde_json::from_str(&json).unwrap();
    key.verify_manifest(&decoded, &record.canonical_skill_dir)
        .unwrap();
    assert_eq!(key.public_key().len(), 32);
    for field in [
        "createdAt",
        "updatedAt",
        "policy",
        "manifestHash",
        "previousManifestSignature",
    ] {
        let mut value = serde_json::to_value(&record).unwrap();
        value[field] = serde_json::json!("altered");
        let tampered: Manifest = serde_json::from_value(value).unwrap();
        assert!(
            key.verify_manifest(&tampered, &record.canonical_skill_dir)
                .is_err(),
            "{field}"
        );
    }
    let mut bad_signature = decoded;
    bad_signature.signature.as_mut().unwrap().value = "AAAA".into();
    assert!(
        key.verify_manifest(&bad_signature, &record.canonical_skill_dir)
            .is_err()
    );
}

#[test]
fn signature_cannot_be_replayed_to_same_named_skill_or_new_key() {
    let (_dir, path) = state();
    let key = KeyStore::open(path).unwrap().initialize().unwrap();
    let mut record = manifest();
    key.sign_manifest(&mut record).unwrap();
    assert!(
        key.verify_manifest(&record, &SkillIdentity::new("/other/alpha").unwrap())
            .is_err()
    );
    let (_other_dir, other_path) = state();
    let replacement = KeyStore::open(other_path).unwrap().initialize().unwrap();
    assert!(
        replacement
            .verify_manifest(&record, &record.canonical_skill_dir)
            .is_err()
    );
}

#[test]
fn v1_record_without_canonical_binding_is_not_imported() {
    let mut value = serde_json::to_value(manifest()).unwrap();
    value.as_object_mut().unwrap().remove("canonicalSkillDir");
    value["version"] = serde_json::json!(1);
    assert!(serde_json::from_value::<Manifest>(value).is_err());
}

#[test]
fn signing_rejects_unsafe_manifest_paths_and_invalid_decisions() {
    let (_dir, path) = state();
    let key = KeyStore::open(path).unwrap().initialize().unwrap();
    for name in [
        "../escape",
        "/etc/passwd",
        ".skill-meta/latest.json",
        "a//b",
        "a/./b",
        "a/../b",
    ] {
        let mut record = manifest();
        record
            .file_hashes
            .insert(name.into(), format!("sha256:{}", "0".repeat(64)));
        assert!(key.sign_manifest(&mut record).is_err());
    }
    let mut value = serde_json::to_value(manifest()).unwrap();
    value["userDecision"] =
        serde_json::json!({"action":"rollback", "targetVersionId":null, "reason":null});
    let mut record: Manifest = serde_json::from_value(value).unwrap();
    assert!(key.sign_manifest(&mut record).is_err());
}

#[test]
fn findings_cannot_be_hidden_by_a_lower_scanner_summary() {
    let (_dir, path) = state();
    let key = KeyStore::open(path).unwrap().initialize().unwrap();
    let mut value = serde_json::to_value(manifest()).unwrap();
    value["scans"] = serde_json::json!([{
        "scanner": "skill-vetter", "version": "1", "status": "pass",
        "findings": [{"rule": "unsafe", "level": "deny", "message": "synthetic"}],
        "scannedAt": "2026-09-15T00:00:00Z"
    }]);
    value["scanStatus"] = serde_json::json!("pass");
    let mut record: Manifest = serde_json::from_value(value).unwrap();
    assert!(key.sign_manifest(&mut record).is_err());
    // Coverage gaps may raise severity even when no content finding was emitted.
    record.scans[0].findings.clear();
    record.scans[0].status = asc_capability_skill_sec::ScanStatus::Warn;
    record.scan_status = asc_capability_skill_sec::ScanStatus::Warn;
    key.sign_manifest(&mut record).unwrap();
}

#[test]
fn initialization_is_idempotent_even_when_calls_race() {
    let (_dir, path) = state();
    let store = Arc::new(KeyStore::open(&path).unwrap());
    let tasks: Vec<_> = (0..4)
        .map(|_| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || store.initialize().unwrap().fingerprint())
        })
        .collect();
    let expected = store.initialize().unwrap().fingerprint();
    for task in tasks {
        assert_eq!(task.join().unwrap(), expected);
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
    assert_eq!(
        fs::metadata(path.join("signing-key.pk8"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn corrupt_or_permissive_keys_are_not_automatically_replaced() {
    let (_dir, path) = state();
    let store = KeyStore::open(&path).unwrap();
    store.initialize().unwrap();
    let key = path.join("signing-key.pk8");
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load().is_err());
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&key, "corrupt").unwrap();
    assert!(store.initialize().is_err());
    assert_eq!(fs::read(&key).unwrap(), b"corrupt");
    fs::write(&key, vec![0_u8; 4097]).unwrap();
    assert!(matches!(
        store.load(),
        Err(asc_capability_skill_sec::SkillSecError::Invalid(_))
    ));
}

#[test]
#[ignore = "requires root in the isolated Linux acceptance container"]
fn foreign_owned_key_and_key_directory_are_rejected() {
    assert!(rustix::process::geteuid().is_root());
    let (_dir, path) = state();
    let store = KeyStore::open(&path).unwrap();
    store.initialize().unwrap();
    let foreign_uid = rustix::process::Uid::from_raw(65534);
    rustix::fs::chown(path.join("signing-key.pk8"), Some(foreign_uid), None).unwrap();
    assert!(store.load().is_err());
    rustix::fs::chown(&path, Some(foreign_uid), None).unwrap();
    assert!(KeyStore::open(&path).is_err());
}

#[test]
fn symlinked_or_hard_linked_keys_and_permissive_state_are_rejected() {
    let (_dir, path) = state();
    let store = KeyStore::open(&path).unwrap();
    store.initialize().unwrap();
    let key = path.join("signing-key.pk8");
    let other = path.join("other");
    fs::hard_link(&key, &other).unwrap();
    assert!(store.load().is_err());
    fs::remove_file(&key).unwrap();
    symlink(&other, &key).unwrap();
    assert!(store.load().is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(KeyStore::open(&path).is_err());
}

#[test]
fn source_exclusions_and_snapshot_strictness_match_the_contract() {
    let (_dir, path) = state();
    fs::write(path.join("SKILL.md"), "hello").unwrap();
    fs::create_dir(path.join(".skill-meta")).unwrap();
    fs::write(path.join(".skill-meta/latest.json"), "ignored").unwrap();
    fs::create_dir_all(path.join("nested/.git")).unwrap();
    fs::write(path.join("nested/.git/config"), "ignored").unwrap();
    symlink("/etc/passwd", path.join("outside")).unwrap();
    let hashes = hash_tree(&path, false).unwrap();
    assert_eq!(hashes.len(), 1);
    assert_eq!(
        hashes["SKILL.md"],
        "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert!(hash_tree(&path, true).is_err());
    fs::remove_dir_all(path.join(".skill-meta")).unwrap();
    assert!(hash_tree(&path, true).is_err());
    fs::remove_dir_all(path.join("nested/.git")).unwrap();
    fs::remove_file(path.join("outside")).unwrap();
    assert_eq!(hash_tree(&path, true).unwrap(), hashes);
    fs::write(path.join("SKILL.md"), "changed").unwrap();
    fs::write(path.join("new"), "new").unwrap();
    let diff = HashDiff::between(&hashes, &hash_tree(&path, false).unwrap());
    assert!(!diff.matches);
    assert_eq!(diff.modified, ["SKILL.md"]);
    assert_eq!(diff.added, ["new"]);
    fs::remove_file(path.join("SKILL.md")).unwrap();
    let diff = HashDiff::between(&hashes, &hash_tree(&path, false).unwrap());
    assert_eq!(diff.removed, ["SKILL.md"]);
}

#[test]
fn special_files_never_block_source_or_snapshot_hashing() {
    let (_dir, path) = state();
    let fifo = path.join("fifo");
    rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RUSR).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(path.join("socket")).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let source = hash_tree(&path, false);
        let snapshot = hash_tree(&path, true);
        sender.send((source, snapshot)).unwrap();
    });
    let (source, snapshot) = receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("special file hashing must not wait for a writer");
    assert!(source.unwrap().is_empty());
    assert!(snapshot.is_err());
}

#[test]
fn a_symlinked_io_root_is_not_followed_implicitly() {
    let (_dir, path) = state();
    let link = path.join("link");
    symlink(&path, &link).unwrap();
    assert!(hash_tree(&link, false).is_err());
    // Canonical identity may contain a source symlink, but the resolver must
    // explicitly supply its verified physical I/O root before hashing.
    assert!(SkillIdentity::new(&link).is_ok());
    fs::create_dir(path.join("child")).unwrap();
    assert!(hash_tree(&link.join("child"), false).is_err());
    assert!(KeyStore::open(link.join("child")).is_err());
}
