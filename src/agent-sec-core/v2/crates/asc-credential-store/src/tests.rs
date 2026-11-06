use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use crate::{
    Carrier, CredentialId, CredentialSpec, CredentialState, CredentialStore, CredentialStoreError,
    FakeFormat, MAX_ACTIVE_FAKES_PER_SCOPE, STORE_FILE_NAME, ScopeId, Secret, SecretsTarget,
};

const REAL: &str = "ghp_0123456789abcdefghijABCDEFGHIJ012345";

fn id(value: &str) -> CredentialId {
    CredentialId::new(value).unwrap()
}

fn scope(value: &str) -> ScopeId {
    ScopeId::new(value).unwrap()
}

fn spec(value: &str) -> CredentialSpec {
    CredentialSpec {
        id: id(value),
        carriers: vec![Carrier::HeaderToken, Carrier::BasicPassword],
        format: FakeFormat::with_prefixes(["ghp_", "github_pat_"]),
    }
}

fn secret(value: &str) -> Secret {
    Secret::new(value.into()).unwrap()
}

fn numbered_real(index: usize) -> String {
    format!("ghp_{index:036}")
}

struct Fixture {
    _root: tempfile::TempDir,
    store_dir: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let store_dir = root.path().join("credentials");
    Fixture {
        _root: root,
        store_dir,
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn store_text(fixture: &Fixture) -> String {
    fs::read_to_string(fixture.store_dir.join(STORE_FILE_NAME)).unwrap()
}

#[test]
fn open_never_creates_the_store() {
    let fixture = fixture();
    let error = CredentialStore::open(&fixture.store_dir).unwrap_err();
    assert!(matches!(error, CredentialStoreError::NotEnabled { .. }));
    assert!(!fixture.store_dir.exists());
}

#[test]
fn create_uses_owner_only_modes_and_survives_reopen() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    assert_eq!(mode(&fixture.store_dir), 0o700);
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    assert_eq!(mode(&fixture.store_dir.join(STORE_FILE_NAME)), 0o600);

    assert!(matches!(
        CredentialStore::open(&fixture.store_dir),
        Err(CredentialStoreError::Locked { .. })
    ));
    let listed = store.list();
    let persisted: serde_json::Value = serde_json::from_str(&store_text(&fixture)).unwrap();
    assert_eq!(persisted["credentials"][0]["fakes"][0]["scopeId"], "dev");
    assert_eq!(
        serde_json::to_value(&listed).unwrap()[0]["fakes"][0]["scopeId"],
        "dev"
    );
    drop(store);

    let reopened = CredentialStore::open(&fixture.store_dir).unwrap();
    assert_eq!(
        reopened.fake(&id("github"), &scope("dev")),
        Some(fake.as_str())
    );
    assert_eq!(reopened.list(), listed);
}

#[test]
fn fakes_are_reused_per_scope_and_isolated_between_scopes() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    assert!(store.list()[0].fakes.is_empty());
    assert_eq!(store.fake(&id("github"), &scope("a")), None);
    let first = store.issue_fake(&id("github"), &scope("a")).unwrap();
    assert_eq!(store.issue_fake(&id("github"), &scope("a")).unwrap(), first);
    let other = store.issue_fake(&id("github"), &scope("b")).unwrap();
    assert_ne!(first, other);
    assert_ne!(first, REAL);
    assert!(first.starts_with("ghp_") && first.len() == REAL.len());
}

#[test]
fn identical_import_is_a_no_op_and_active_value_or_spec_changes_are_rejected() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    let before = store_text(&fixture);
    let listed = store.list();
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    let inode = fs::metadata(&path).unwrap().ino();
    let mut reordered = spec("github");
    reordered.carriers = vec![
        Carrier::BasicPassword,
        Carrier::HeaderToken,
        Carrier::BasicPassword,
    ];
    for unchanged in [spec("github"), reordered] {
        store.put(unchanged, secret(REAL)).unwrap();
        assert_eq!(store_text(&fixture), before);
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    }

    let mut changed_carriers = spec("github");
    changed_carriers.carriers = vec![Carrier::HeaderToken];
    let mut changed_format = spec("github");
    changed_format.format = FakeFormat::with_prefixes(["ghp_"]);
    for (changed, value) in [
        (spec("github"), numbered_real(7)),
        (changed_carriers, REAL.to_owned()),
        (changed_format, REAL.to_owned()),
    ] {
        let error = store.put(changed, secret(&value)).unwrap_err();
        assert!(matches!(
            error,
            CredentialStoreError::Conflict(ref credential) if credential == &id("github")
        ));
        assert!(!error.to_string().contains(&value));
        assert_eq!(store_text(&fixture), before);
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(store.list(), listed);
        assert_eq!(
            store.issue_fake(&id("github"), &scope("dev")).unwrap(),
            fake
        );
    }
    drop(store);
    let mut reopened = CredentialStore::open(&fixture.store_dir).unwrap();
    assert_eq!(reopened.list(), listed);
    assert_eq!(
        reopened.issue_fake(&id("github"), &scope("dev")).unwrap(),
        fake
    );
    assert_eq!(store_text(&fixture), before);
}

#[test]
fn duplicate_real_values_and_fake_reuse_as_real_are_rejected() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    assert!(matches!(
        store.put(spec("other"), secret(REAL)),
        Err(CredentialStoreError::DuplicateValue(_))
    ));
    assert!(matches!(
        store.put(spec("other"), secret(&fake)),
        Err(CredentialStoreError::DuplicateValue(_))
    ));
}

#[test]
fn remove_and_reimport_keep_old_fakes_as_persisted_rejection_tombstones() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let old = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    store.remove(&id("github")).unwrap();
    store.remove(&id("github")).unwrap();
    let retired = store_text(&fixture);
    assert!(!retired.contains(REAL));
    assert_eq!(store.fake(&id("github"), &scope("dev")), None);
    assert!(matches!(
        store.issue_fake(&id("github"), &scope("dev")),
        Err(CredentialStoreError::Retired(_))
    ));
    let summary = &store.list()[0];
    assert_eq!(summary.state, CredentialState::Retired);
    assert!(summary.fakes[0].retired);
    assert!(matches!(
        store.put(spec("github"), secret(&old)),
        Err(CredentialStoreError::DuplicateValue(_))
    ));
    assert_eq!(store_text(&fixture), retired);
    drop(store);

    let mut store = CredentialStore::open(&fixture.store_dir).unwrap();
    let real = numbered_real(1);
    let mut reimported = spec("github");
    reimported.carriers = vec![Carrier::HeaderToken];
    reimported.format = FakeFormat::with_prefixes(["ghp_"]);
    store.put(reimported, secret(&real)).unwrap();
    assert_eq!(store.fake(&id("github"), &scope("dev")), None);
    let new = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    assert_ne!(new, old);
    let listed = store.list();
    let persisted = store_text(&fixture);
    drop(store);

    let mut store = CredentialStore::open(&fixture.store_dir).unwrap();
    assert_eq!(store.list(), listed);
    assert_eq!(store.fake(&id("github"), &scope("dev")), Some(new.as_str()));
    assert!(matches!(
        store.put(spec("other"), secret(&old)),
        Err(CredentialStoreError::DuplicateValue(_))
    ));
    assert_eq!(store_text(&fixture), persisted);
    let run = fixture.store_dir.with_file_name("run");
    fs::create_dir(&run).unwrap();
    let target = SecretsTarget {
        directory: run.clone(),
        file_name: "secrets.json".into(),
        group: None,
    };
    store.materialize(&target, &[id("github")], 1).unwrap();
    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(run.join("secrets.json")).unwrap()).unwrap();
    assert_eq!(
        snapshot,
        serde_json::json!({
            "version": 1,
            "generation": 1,
            "entries": [{
                "fake": new,
                "credential": "github",
                "scopeId": "dev",
                "real": real,
                "carriers": ["header_token"]
            }],
            "retired": [{"fake": old, "credential": "github", "scopeId": "dev"}]
        })
    );
    assert!(matches!(
        store.remove(&id("missing")),
        Err(CredentialStoreError::UnknownCredential(_))
    ));
}

#[test]
fn retiring_one_scopes_fake_keeps_the_credential_for_other_scopes() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let dev = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    let ops = store.issue_fake(&id("github"), &scope("ops")).unwrap();
    store.retire_fake(&id("github"), &scope("dev")).unwrap();
    store.retire_fake(&id("github"), &scope("dev")).unwrap();
    assert_eq!(store.fake(&id("github"), &scope("dev")), None);
    assert_eq!(store.fake(&id("github"), &scope("ops")), Some(ops.as_str()));
    let summary = &store.list()[0];
    assert_eq!(summary.state, CredentialState::Active);
    assert_eq!(
        summary
            .fakes
            .iter()
            .map(|fake| (fake.scope_id.as_str(), fake.retired))
            .collect::<Vec<_>>(),
        [("ops", false), ("dev", true)]
    );
    let reissued = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    assert_ne!(reissued, dev);
    assert!(matches!(
        store.retire_fake(&id("missing"), &scope("dev")),
        Err(CredentialStoreError::UnknownCredential(_))
    ));
}

#[test]
fn per_scope_capacity_is_enforced() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    for index in 0..=MAX_ACTIVE_FAKES_PER_SCOPE {
        let name = format!("c{index}");
        store
            .put(spec(&name), secret(&numbered_real(index)))
            .unwrap();
        let issued = store.issue_fake(&id(&name), &scope("dev"));
        if index < MAX_ACTIVE_FAKES_PER_SCOPE {
            issued.unwrap();
        } else {
            assert!(matches!(issued, Err(CredentialStoreError::Capacity(_))));
        }
    }
    store.issue_fake(&id("c0"), &scope("other")).unwrap();
}

#[test]
fn values_never_leak_through_debug_list_or_errors() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    let listed = serde_json::to_string(&store.list()).unwrap();
    let debug = format!("{store:?}");
    for text in [&listed, &debug] {
        assert!(!text.contains(REAL) && !text.contains(&fake), "{text}");
    }

    drop(store);
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    let broken =
        store_text(&fixture).replace("\"version\": 1", &format!("\"version\": \"{REAL}\""));
    fs::write(&path, broken).unwrap();
    let error = CredentialStore::open(&fixture.store_dir).unwrap_err();
    assert!(matches!(error, CredentialStoreError::Corrupt(_)));
    assert!(!error.to_string().contains(REAL), "{error}");
}

#[test]
fn unsafe_paths_are_refused() {
    let fixture = fixture();
    CredentialStore::create(&fixture.store_dir).unwrap();

    fs::set_permissions(&fixture.store_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        CredentialStore::open(&fixture.store_dir),
        Err(CredentialStoreError::UnsafePath { .. })
    ));
    fs::set_permissions(&fixture.store_dir, fs::Permissions::from_mode(0o700)).unwrap();

    let link = fixture.store_dir.with_file_name("link");
    symlink(&fixture.store_dir, &link).unwrap();
    assert!(matches!(
        CredentialStore::open(&link),
        Err(CredentialStoreError::UnsafePath { .. })
    ));

    let decoy = fixture.store_dir.with_file_name("decoy.json");
    fs::write(&decoy, "{}").unwrap();
    symlink(&decoy, fixture.store_dir.join(STORE_FILE_NAME)).unwrap();
    assert!(matches!(
        CredentialStore::open(&fixture.store_dir),
        Err(CredentialStoreError::UnsafePath { .. })
    ));
}

#[test]
fn intermediate_symlinks_and_missing_parents_are_refused() {
    let fixture = fixture();
    CredentialStore::create(&fixture.store_dir).unwrap();
    let parent = fixture.store_dir.parent().unwrap().to_path_buf();
    let alias = parent.with_file_name(format!(
        "{}-alias",
        parent.file_name().unwrap().to_str().unwrap()
    ));
    symlink(&parent, &alias).unwrap();
    let through_alias = CredentialStore::open(&alias.join("credentials"));
    fs::remove_file(&alias).unwrap();
    assert!(matches!(
        through_alias,
        Err(CredentialStoreError::UnsafePath { .. })
    ));

    let missing = parent.join("missing");
    assert!(CredentialStore::create(&missing.join("credentials")).is_err());
    assert!(!missing.exists());
}

#[test]
fn hard_linked_store_file_is_refused() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    drop(store);
    fs::hard_link(
        fixture.store_dir.join(STORE_FILE_NAME),
        fixture.store_dir.with_file_name("copy"),
    )
    .unwrap();
    assert!(matches!(
        CredentialStore::open(&fixture.store_dir),
        Err(CredentialStoreError::UnsafePath { .. })
    ));
}

#[test]
fn leftover_temporary_files_are_removed_on_open() {
    let fixture = fixture();
    drop(CredentialStore::create(&fixture.store_dir).unwrap());
    let leftover = fixture.store_dir.join(".tmp-0011");
    fs::write(&leftover, REAL).unwrap();
    drop(CredentialStore::open(&fixture.store_dir).unwrap());
    assert!(!leftover.exists());
}

#[test]
fn failed_write_leaves_memory_unchanged() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let before = store.list();
    // Renaming onto a non-empty directory fails even for root.
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("block"), "x").unwrap();

    assert!(store.put(spec("other"), secret(&numbered_real(5))).is_err());
    assert!(store.issue_fake(&id("github"), &scope("dev")).is_err());
    assert!(store.remove(&id("github")).is_err());
    assert_eq!(store.list(), before);
    assert_eq!(store.fake(&id("github"), &scope("dev")), None);
}

#[test]
fn unusable_real_values_are_rejected_at_put() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    assert!(matches!(
        store.put(spec("github"), secret("ghp_short-value-only")),
        Err(CredentialStoreError::Invalid { .. })
    ));
    store.put(spec("github"), secret(REAL)).unwrap();
    let tombstone = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    store.remove(&id("github")).unwrap();
    assert!(matches!(
        store.put(spec("other"), secret(&tombstone)),
        Err(CredentialStoreError::DuplicateValue(_))
    ));
}

#[test]
fn store_file_with_loose_mode_is_refused() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    drop(store);
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        CredentialStore::open(&fixture.store_dir),
        Err(CredentialStoreError::UnsafePath { .. })
    ));
}

#[test]
fn materialize_writes_only_selected_mappings_and_all_tombstones() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let other_real = numbered_real(2);
    let gone_real = numbered_real(3);
    store.put(spec("github"), secret(REAL)).unwrap();
    store.put(spec("other"), secret(&other_real)).unwrap();
    store.put(spec("gone"), secret(&gone_real)).unwrap();
    let fake = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    store.issue_fake(&id("other"), &scope("dev")).unwrap();
    let tombstone = store.issue_fake(&id("gone"), &scope("dev")).unwrap();
    store.remove(&id("gone")).unwrap();

    let run = fixture.store_dir.with_file_name("run");
    fs::create_dir(&run).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o750)).unwrap();
    let target = SecretsTarget {
        directory: run.clone(),
        file_name: "secrets-1.json".into(),
        group: None,
    };
    store.materialize(&target, &[id("github")], 1).unwrap();

    let path = run.join("secrets-1.json");
    assert_eq!(mode(&path), 0o640);
    let snapshot: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(snapshot["version"], 1);
    assert_eq!(snapshot["generation"], 1);
    assert_eq!(snapshot["entries"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["entries"][0]["fake"], fake.as_str());
    assert_eq!(snapshot["entries"][0]["real"], REAL);
    assert_eq!(snapshot["entries"][0]["scopeId"], "dev");
    assert_eq!(snapshot["retired"][0]["fake"], tombstone.as_str());
    let text = snapshot.to_string();
    assert!(!text.contains(&other_real) && !text.contains(&gone_real));
    assert!(
        fs::read_dir(&run).unwrap().count() == 1,
        "temporary file left behind"
    );

    let bad = SecretsTarget {
        file_name: "../escape".into(),
        ..target
    };
    assert!(matches!(
        store.materialize(&bad, &[id("github")], 2),
        Err(CredentialStoreError::Invalid { .. })
    ));
    assert!(matches!(
        store.materialize(
            &SecretsTarget {
                file_name: "s.json".into(),
                ..bad
            },
            &[id("gone")],
            2
        ),
        Err(CredentialStoreError::Retired(_))
    ));
}

#[test]
fn materialize_keeps_original_mapping_after_conflict_and_rejects_unsafe_targets() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &scope("dev")).unwrap();
    let persisted = store_text(&fixture);
    assert!(matches!(
        store.put(spec("github"), secret(&numbered_real(9))),
        Err(CredentialStoreError::Conflict(_))
    ));
    assert_eq!(store_text(&fixture), persisted);

    let run = fixture.store_dir.with_file_name("run");
    fs::create_dir(&run).unwrap();
    let group = fs::metadata(&run).unwrap().gid();
    let target = SecretsTarget {
        directory: run.clone(),
        file_name: "secrets.json".into(),
        group: Some(group),
    };
    store.materialize(&target, &[id("github")], 3).unwrap();
    let path = run.join("secrets.json");
    assert_eq!(fs::metadata(&path).unwrap().gid(), group);
    let snapshot: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        snapshot,
        serde_json::json!({
            "version": 1,
            "generation": 3,
            "entries": [{
                "fake": fake,
                "credential": "github",
                "scopeId": "dev",
                "real": REAL,
                "carriers": ["header_token", "basic_password"]
            }],
            "retired": []
        })
    );

    let into_store = SecretsTarget {
        directory: fixture.store_dir.clone(),
        file_name: STORE_FILE_NAME.into(),
        group: None,
    };
    assert!(matches!(
        store.materialize(&into_store, &[id("github")], 4),
        Err(CredentialStoreError::UnsafePath { .. })
    ));
    assert!(matches!(
        store.materialize(&target, &[id("github"), id("github")], 4),
        Err(CredentialStoreError::Invalid { .. })
    ));
    let unnamed = SecretsTarget {
        file_name: String::new(),
        ..target
    };
    assert!(matches!(
        store.materialize(&unnamed, &[id("github")], 4),
        Err(CredentialStoreError::Invalid { .. })
    ));
}
