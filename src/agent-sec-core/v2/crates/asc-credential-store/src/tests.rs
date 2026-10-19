use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use crate::{
    AgentName, Carrier, CredentialId, CredentialSpec, CredentialState, CredentialStore,
    CredentialStoreError, FakeFormat, MAX_ACTIVE_FAKES_PER_AGENT, PutOutcome, STORE_FILE_NAME,
    Secret, SecretsTarget,
};

const REAL: &str = "ghp_0123456789abcdefghijABCDEFGHIJ012345";

fn id(value: &str) -> CredentialId {
    CredentialId::new(value).unwrap()
}

fn agent(value: &str) -> AgentName {
    AgentName::new(value).unwrap()
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
    assert_eq!(
        store.put(spec("github"), secret(REAL)).unwrap(),
        PutOutcome::Created
    );
    let fake = store.issue_fake(&id("github"), &agent("dev")).unwrap();
    assert_eq!(mode(&fixture.store_dir.join(STORE_FILE_NAME)), 0o600);

    assert!(matches!(
        CredentialStore::open(&fixture.store_dir),
        Err(CredentialStoreError::Locked { .. })
    ));
    let listed = store.list();
    drop(store);

    let reopened = CredentialStore::open(&fixture.store_dir).unwrap();
    assert_eq!(
        reopened.fake(&id("github"), &agent("dev")),
        Some(fake.as_str())
    );
    assert_eq!(reopened.list(), listed);
}

#[test]
fn fakes_are_reused_per_agent_and_isolated_between_agents() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let first = store.issue_fake(&id("github"), &agent("a")).unwrap();
    assert_eq!(store.issue_fake(&id("github"), &agent("a")).unwrap(), first);
    let other = store.issue_fake(&id("github"), &agent("b")).unwrap();
    assert_ne!(first, other);
    assert_ne!(first, REAL);
    assert!(first.starts_with("ghp_") && first.len() == REAL.len());
}

#[test]
fn same_value_put_is_unchanged_and_rotation_keeps_fakes() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &agent("dev")).unwrap();
    assert_eq!(
        store.put(spec("github"), secret(REAL)).unwrap(),
        PutOutcome::Unchanged
    );
    let rotated = numbered_real(7);
    assert_eq!(
        store.put(spec("github"), secret(&rotated)).unwrap(),
        PutOutcome::Replaced
    );
    assert_eq!(
        store.issue_fake(&id("github"), &agent("dev")).unwrap(),
        fake
    );
}

#[test]
fn duplicate_real_values_and_fake_reuse_as_real_are_rejected() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &agent("dev")).unwrap();
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
fn remove_keeps_tombstones_and_reactivation_issues_new_fakes() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let old = store.issue_fake(&id("github"), &agent("dev")).unwrap();
    store.remove(&id("github")).unwrap();
    store.remove(&id("github")).unwrap();
    assert!(!store_text(&fixture).contains(REAL));
    assert!(matches!(
        store.issue_fake(&id("github"), &agent("dev")),
        Err(CredentialStoreError::Retired(_))
    ));
    let summary = &store.list()[0];
    assert_eq!(summary.state, CredentialState::Retired);
    assert!(summary.fakes[0].retired);

    store
        .put(spec("github"), secret(&numbered_real(1)))
        .unwrap();
    let new = store.issue_fake(&id("github"), &agent("dev")).unwrap();
    assert_ne!(new, old);
    assert!(matches!(
        store.remove(&id("missing")),
        Err(CredentialStoreError::UnknownCredential(_))
    ));
}

#[test]
fn retiring_one_agents_fake_keeps_the_credential_for_others() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let dev = store.issue_fake(&id("github"), &agent("dev")).unwrap();
    let ops = store.issue_fake(&id("github"), &agent("ops")).unwrap();
    store.retire_fake(&id("github"), &agent("dev")).unwrap();
    store.retire_fake(&id("github"), &agent("dev")).unwrap();
    assert_eq!(store.fake(&id("github"), &agent("dev")), None);
    assert_eq!(store.fake(&id("github"), &agent("ops")), Some(ops.as_str()));
    let summary = &store.list()[0];
    assert_eq!(summary.state, CredentialState::Active);
    assert_eq!(
        summary
            .fakes
            .iter()
            .map(|fake| (fake.agent.as_str(), fake.retired))
            .collect::<Vec<_>>(),
        [("ops", false), ("dev", true)]
    );
    let reissued = store.issue_fake(&id("github"), &agent("dev")).unwrap();
    assert_ne!(reissued, dev);
    assert!(matches!(
        store.retire_fake(&id("missing"), &agent("dev")),
        Err(CredentialStoreError::UnknownCredential(_))
    ));
}

#[test]
fn per_agent_capacity_is_enforced() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    for index in 0..=MAX_ACTIVE_FAKES_PER_AGENT {
        let name = format!("c{index}");
        store
            .put(spec(&name), secret(&numbered_real(index)))
            .unwrap();
        let issued = store.issue_fake(&id(&name), &agent("dev"));
        if index < MAX_ACTIVE_FAKES_PER_AGENT {
            issued.unwrap();
        } else {
            assert!(matches!(issued, Err(CredentialStoreError::Capacity(_))));
        }
    }
    store.issue_fake(&id("c0"), &agent("other")).unwrap();
}

#[test]
fn values_never_leak_through_debug_list_or_errors() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &agent("dev")).unwrap();
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
    assert!(store.issue_fake(&id("github"), &agent("dev")).is_err());
    assert!(store.remove(&id("github")).is_err());
    assert_eq!(store.list(), before);
    assert_eq!(store.fake(&id("github"), &agent("dev")), None);
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
    let tombstone = store.issue_fake(&id("github"), &agent("dev")).unwrap();
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
    let fake = store.issue_fake(&id("github"), &agent("dev")).unwrap();
    store.issue_fake(&id("other"), &agent("dev")).unwrap();
    let tombstone = store.issue_fake(&id("gone"), &agent("dev")).unwrap();
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
    assert_eq!(snapshot["entries"][0]["agent"], "dev");
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
fn materialize_follows_rotation_and_rejects_unsafe_targets() {
    use std::os::unix::fs::MetadataExt as _;

    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.put(spec("github"), secret(REAL)).unwrap();
    let fake = store.issue_fake(&id("github"), &agent("dev")).unwrap();
    let rotated = numbered_real(9);
    store.put(spec("github"), secret(&rotated)).unwrap();

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
    assert_eq!(snapshot["entries"][0]["fake"], fake.as_str());
    assert_eq!(snapshot["entries"][0]["real"], rotated.as_str());

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
