//! Behavior gates for pair identity, lifecycle, publication and filesystem trust boundaries.

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use crate::{
    CredentialId, CredentialState, CredentialStore, CredentialStoreError, FakeFormat,
    MAX_ACTIVE_FAKES_PER_SCOPE, MAX_FAKES_TOTAL, PairInput, STORE_FILE_NAME, ScopeId, Secret,
    SecretsTarget, generate_fake, read_token_file, validate_pair, write_fake_file,
};

const REAL: &str = "aaaaaaaaaaaaaaaaaaaa";
const FAKE: &str = "bbbbbbbbbbbbbbbbbbbb";
const OTHER: &str = "cccccccccccccccccccc";
const SOURCE_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn id(value: &str) -> CredentialId {
    CredentialId::new(value).unwrap()
}

fn scope(value: &str) -> ScopeId {
    ScopeId::new(value).unwrap()
}

fn pair(real: &str, fake: &str) -> PairInput {
    PairInput {
        real: Secret::new(real.into()).unwrap(),
        fake: fake.into(),
    }
}

fn numbered_pair(index: usize) -> PairInput {
    let real = format!("a{index:019}");
    let fake: String = real
        .bytes()
        .map(|byte| {
            char::from(if byte.is_ascii_digit() {
                b'0' + (byte - b'0' + 5) % 10
            } else {
                b'b'
            })
        })
        .collect();
    pair(&real, &fake)
}

struct Fixture {
    root: tempfile::TempDir,
    store_dir: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let store_dir = root.path().join("credentials");
    Fixture { root, store_dir }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn store_text(fixture: &Fixture) -> String {
    fs::read_to_string(fixture.store_dir.join(STORE_FILE_NAME)).unwrap()
}

fn target(fixture: &Fixture) -> SecretsTarget {
    let directory = fixture.root.path().join("run");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
    SecretsTarget {
        directory,
        file_name: "secrets.json".into(),
        group: None,
    }
}

fn snapshot(target: &SecretsTarget) -> serde_json::Value {
    serde_json::from_slice(&fs::read(target.directory.join(&target.file_name)).unwrap()).unwrap()
}

#[test]
fn pair_validation_counts_every_position_with_exact_integer_rounding() {
    for (real, fake, valid) in [
        ("a", "b", true),
        ("a", "a", false),
        ("aa", "ab", false),
        ("aaaaa", "abbbb", true),
        ("aaaaa", "aabbb", false),
        ("aaaaaa", "aabbbb", false),
        ("aaaaaa", "abbbbb", true),
        ("ab-12", "ab-34", false),
        ("ab-12", "cd-34", true),
        ("abc", "defg", false),
        ("", "", false),
        ("has space", "different", false),
        ("é", "aa", false),
        ("a\n", "bb", false),
        ("aa", "b\t", false),
    ] {
        assert_eq!(
            validate_pair(real, fake).is_ok(),
            valid,
            "{real:?}, {fake:?}"
        );
    }
    assert!(validate_pair(&"a".repeat(512), &"b".repeat(512)).is_ok());
    assert!(validate_pair(&"a".repeat(513), &"b".repeat(513)).is_err());
}

#[test]
fn auto_generation_changes_each_random_position_and_preserves_only_known_shape() {
    for prefix in ["ghp_", "github_pat_", "gho_", "ghu_", "ghs_", "ghr_", ""] {
        let real = format!(
            "{prefix}{}",
            "aZ0bY1cX2dW3eV4fU5gT6hS7iR8jQ9kP0lN1mM2".repeat(2)
        );
        let fake = generate_fake(&real, "auto").unwrap();
        validate_pair(&real, &fake).unwrap();
        assert!(fake.starts_with(prefix));
        for (a, b) in real
            .bytes()
            .skip(prefix.len())
            .zip(fake.bytes().skip(prefix.len()))
        {
            assert_ne!(a, b);
            assert_eq!(a.is_ascii_lowercase(), b.is_ascii_lowercase());
            assert_eq!(a.is_ascii_uppercase(), b.is_ascii_uppercase());
            assert_eq!(a.is_ascii_digit(), b.is_ascii_digit());
        }
    }
    let exact = "ghp_0123456789012345";
    validate_pair(exact, &generate_fake(exact, "auto").unwrap()).unwrap();
    let structured = "ghp_01234567890123456789_ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let fake = generate_fake(structured, "auto").unwrap();
    assert_eq!(fake.as_bytes()[24], b'_');
    let helper = FakeFormat::with_prefixes(["p_", "p"]);
    let real = "p_0123456789012345__";
    let fake = helper
        .generate(real, &ring::rand::SystemRandom::new())
        .unwrap();
    assert!(fake.starts_with("p_") && fake.ends_with("__"));
    validate_pair(real, &fake).unwrap();
    assert!(
        helper
            .generate("p_0123456789012345___", &ring::rand::SystemRandom::new())
            .is_err()
    );
}

#[test]
fn generation_refuses_guessing_structured_or_insufficiently_different_values() {
    for real in [
        "ghp_short",
        "012345678901234",
        "github_pat_0123456789012345",
        "ghp_0123456789012345________",
        "sk-012345678901234567890123456789",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature",
        "unknown_012345678901234567890123456789",
        "0123456789012345\n",
    ] {
        let error = generate_fake(real, "auto").unwrap_err();
        assert!(!error.to_string().contains(real));
    }
    assert!(generate_fake(REAL, "github").is_err());
    assert!(generate_fake(REAL, "").is_err());
}

#[test]
fn registration_reuses_opaque_identity_in_order_across_reopen_without_rewriting() {
    let fixture = fixture();
    assert!(matches!(
        CredentialStore::open(&fixture.store_dir),
        Err(CredentialStoreError::NotEnabled { .. })
    ));
    assert!(!fixture.store_dir.exists());
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_pairs(vec![pair(REAL, FAKE), pair(REAL, OTHER), pair(REAL, FAKE)])
        .unwrap();
    assert_eq!(ids[0], ids[2]);
    assert_ne!(ids[0], ids[1]);
    for identifier in &ids {
        assert!(identifier.as_str().starts_with("pair-"));
        assert_eq!(identifier.as_str().len(), 37);
        assert!(
            identifier.as_str()[5..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }
    assert_eq!(mode(&fixture.store_dir), 0o700);
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    assert_eq!(mode(&path), 0o600);
    assert!(matches!(
        CredentialStore::open(&fixture.store_dir),
        Err(CredentialStoreError::Locked { .. })
    ));
    assert_eq!(store.fake(&ids[0], &scope("dev")), None);
    store.sync_scopes(&[]).unwrap();
    assert!(
        store
            .list()
            .iter()
            .all(|summary| summary.state == CredentialState::Active && summary.fakes.is_empty())
    );
    let before = store_text(&fixture);
    let inode = fs::metadata(&path).unwrap().ino();
    assert_eq!(
        store
            .register_pairs(vec![pair(REAL, OTHER), pair(REAL, FAKE)])
            .unwrap(),
        vec![ids[1].clone(), ids[0].clone()]
    );
    assert!(store.register_pairs(vec![]).unwrap().is_empty());
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store_text(&fixture), before);
    let listed = store.list();
    drop(store);
    let mut reopened = CredentialStore::open(&fixture.store_dir).unwrap();
    assert_eq!(
        reopened.register_pairs(vec![pair(REAL, FAKE)]).unwrap(),
        vec![ids[0].clone()]
    );
    assert_eq!(reopened.list(), listed);
    assert_eq!(store_text(&fixture), before);
}

#[test]
fn policy_registration_reopens_without_input_files_and_preserves_order_and_shared_pairs() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let manual = store.register_pairs(vec![pair(REAL, FAKE)]).unwrap();
    assert_eq!(store.lookup_registration(SOURCE_KEY).unwrap(), None);
    let real_file = fixture.root.path().join("real-token");
    let fake_file = fixture.root.path().join("fake-token");
    let input = numbered_pair(1);
    fs::write(&real_file, input.real.expose()).unwrap();
    fs::write(&fake_file, &input.fake).unwrap();
    let ids = store
        .register_policy_pairs(
            SOURCE_KEY,
            vec![
                pair(REAL, FAKE),
                PairInput {
                    real: Secret::new(fs::read_to_string(&real_file).unwrap()).unwrap(),
                    fake: fs::read_to_string(&fake_file).unwrap(),
                },
                pair(REAL, FAKE),
            ],
        )
        .unwrap();
    assert_eq!(ids[0], manual[0]);
    assert_eq!(ids[0], ids[2]);
    assert_ne!(ids[0], ids[1]);
    let shared_key = "b".repeat(64);
    assert_eq!(
        store
            .register_policy_pairs(&shared_key, vec![pair(REAL, FAKE)])
            .unwrap(),
        manual
    );
    assert_eq!(store.list().len(), 2);
    store.sync_scopes(&[]).unwrap();
    assert_eq!(store.fake(&ids[0], &scope("a")), None);
    let persisted: serde_json::Value = serde_json::from_str(&store_text(&fixture)).unwrap();
    assert_eq!(persisted["version"], 2);
    assert_eq!(
        persisted["registrations"],
        serde_json::json!({
            SOURCE_KEY: ids, (shared_key.as_str()): manual,
        })
    );
    fs::remove_file(real_file).unwrap();
    fs::remove_file(fake_file).unwrap();
    let before = store_text(&fixture);
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    let inode = fs::metadata(&path).unwrap().ino();
    drop(store);
    let mut reopened = CredentialStore::open(&fixture.store_dir).unwrap();
    assert_eq!(
        reopened.lookup_registration(SOURCE_KEY).unwrap(),
        Some(ids.clone())
    );
    assert_eq!(
        reopened.lookup_registration(&shared_key).unwrap(),
        Some(manual)
    );
    assert_eq!(
        reopened
            .register_policy_pairs(
                SOURCE_KEY,
                vec![pair(REAL, FAKE), numbered_pair(1), pair(REAL, FAKE),]
            )
            .unwrap(),
        ids
    );
    assert_eq!(store_text(&fixture), before);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
}

#[test]
fn repeated_policy_registration_validates_every_pair_and_rejects_receipt_changes_without_writes() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_policy_pairs(SOURCE_KEY, vec![pair(REAL, FAKE), numbered_pair(1)])
        .unwrap();
    let before = store_text(&fixture);
    let listed = store.list();
    for inputs in [
        vec![],
        vec![pair(REAL, FAKE)],
        vec![pair(REAL, FAKE), numbered_pair(1), pair(REAL, FAKE)],
        vec![numbered_pair(1), pair(REAL, FAKE)],
        vec![pair(REAL, FAKE), pair(OTHER, FAKE)],
        vec![pair(REAL, FAKE), pair(REAL, OTHER)],
        vec![pair(REAL, FAKE), pair(REAL, REAL)],
        vec![numbered_pair(9), pair(REAL, "short")],
    ] {
        let error = store.register_policy_pairs(SOURCE_KEY, inputs).unwrap_err();
        assert!(!error.to_string().contains(REAL) && !error.to_string().contains(FAKE));
        assert_eq!(store.list(), listed);
        assert_eq!(
            store.lookup_registration(SOURCE_KEY).unwrap(),
            Some(ids.clone())
        );
        assert_eq!(store_text(&fixture), before);
    }
    let new_key = "b".repeat(64);
    assert!(
        store
            .register_policy_pairs(&new_key, vec![numbered_pair(9), pair(REAL, "short")])
            .is_err()
    );
    assert_eq!(store.lookup_registration(&new_key).unwrap(), None);
    assert_eq!(store.list(), listed);
    assert_eq!(store_text(&fixture), before);
}

#[test]
fn policy_receipts_survive_scope_changes_but_never_resurrect_retired_fakes() {
    for retire_by_sync in [false, true] {
        let fixture = fixture();
        let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
        let ids = store
            .register_policy_pairs(SOURCE_KEY, vec![pair(REAL, FAKE)])
            .unwrap();
        let shared_key = "b".repeat(64);
        store
            .register_policy_pairs(&shared_key, vec![pair(REAL, FAKE)])
            .unwrap();
        store
            .sync_scopes(&[(scope("a"), ids.clone()), (scope("b"), ids.clone())])
            .unwrap();
        store.sync_scopes(&[(scope("b"), ids.clone())]).unwrap();
        assert_eq!(
            store.lookup_registration(SOURCE_KEY).unwrap(),
            Some(ids.clone())
        );
        assert_eq!(
            store.lookup_registration(&shared_key).unwrap(),
            Some(ids.clone())
        );
        assert_eq!(store.fake(&ids[0], &scope("b")), Some(FAKE));
        if retire_by_sync {
            store.sync_scopes(&[]).unwrap();
        } else {
            store.remove(&ids[0]).unwrap();
        }
        let before = store_text(&fixture);
        assert!(!before.contains(REAL));
        let persisted: serde_json::Value = serde_json::from_str(&before).unwrap();
        assert_eq!(
            persisted["registrations"],
            serde_json::json!({SOURCE_KEY: ids, (shared_key.as_str()): ids})
        );
        assert!(matches!(
            store.lookup_registration(SOURCE_KEY),
            Err(CredentialStoreError::Retired(_))
        ));
        drop(store);
        let mut reopened = CredentialStore::open(&fixture.store_dir).unwrap();
        for key in [SOURCE_KEY, shared_key.as_str()] {
            assert!(matches!(
                reopened.lookup_registration(key),
                Err(CredentialStoreError::Retired(_))
            ));
            for input in [pair(REAL, FAKE), pair(REAL, OTHER)] {
                assert!(matches!(
                    reopened.register_policy_pairs(key, vec![input]),
                    Err(CredentialStoreError::Retired(_))
                ));
            }
        }
        assert!(matches!(
            reopened.register_policy_pairs(&"c".repeat(64), vec![pair(REAL, FAKE)]),
            Err(CredentialStoreError::Retired(_))
        ));
        assert!(matches!(
            reopened.sync_scopes(&[(scope("a"), ids.clone())]),
            Err(CredentialStoreError::Retired(_))
        ));
        assert_eq!(reopened.fake(&ids[0], &scope("b")), None);
        assert_eq!(store_text(&fixture), before);
    }
}

#[test]
fn policy_registration_enforces_key_batch_and_receipt_limits_without_limiting_manual_pairs() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    for key in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        assert!(matches!(
            store.lookup_registration(&key),
            Err(CredentialStoreError::Invalid { .. })
        ));
        assert!(matches!(
            store.register_policy_pairs(&key, vec![pair(REAL, FAKE)]),
            Err(CredentialStoreError::Invalid { .. })
        ));
    }
    for length in [0, 257] {
        assert!(matches!(
            store
                .register_policy_pairs(SOURCE_KEY, (0..length).map(|_| pair(REAL, FAKE)).collect()),
            Err(CredentialStoreError::Invalid { .. })
        ));
    }
    assert!(store.list().is_empty());
    assert!(!fixture.store_dir.join(STORE_FILE_NAME).exists());
    let ids = store
        .register_policy_pairs(SOURCE_KEY, (0..256).map(|_| pair(REAL, FAKE)).collect())
        .unwrap();
    assert_eq!(ids, vec![ids[0].clone(); 256]);
    assert_eq!(
        store.lookup_registration(SOURCE_KEY).unwrap(),
        Some(ids.clone())
    );
    let mut persisted: serde_json::Value = serde_json::from_str(&store_text(&fixture)).unwrap();
    let registrations: serde_json::Map<String, serde_json::Value> = (0..4096)
        .map(|index| (format!("{index:064x}"), serde_json::json!([ids[0]])))
        .collect();
    persisted["registrations"] = registrations.into();
    drop(store);
    fs::write(
        fixture.store_dir.join(STORE_FILE_NAME),
        serde_json::to_vec(&persisted).unwrap(),
    )
    .unwrap();
    let mut reopened = CredentialStore::open(&fixture.store_dir).unwrap();
    let before = store_text(&fixture);
    assert_eq!(
        reopened
            .register_policy_pairs(&format!("{:064x}", 0), vec![pair(REAL, FAKE)])
            .unwrap(),
        vec![ids[0].clone()]
    );
    assert!(matches!(
        reopened.register_policy_pairs(SOURCE_KEY, vec![pair(REAL, FAKE)]),
        Err(CredentialStoreError::Capacity(_))
    ));
    assert_eq!(reopened.lookup_registration(SOURCE_KEY).unwrap(), None);
    assert_eq!(store_text(&fixture), before);
    reopened.register_pairs(vec![numbered_pair(1)]).unwrap();
    assert_eq!(reopened.list().len(), 2);
    let after: serde_json::Value = serde_json::from_str(&store_text(&fixture)).unwrap();
    assert_eq!(after["registrations"], persisted["registrations"]);
}

#[test]
fn registration_collisions_and_invalid_batches_leave_disk_and_memory_unchanged() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store.register_pairs(vec![pair(REAL, FAKE)]).unwrap();
    let before = store_text(&fixture);
    let listed = store.list();
    for input in [
        pair(OTHER, FAKE),
        pair(FAKE, OTHER),
        pair(OTHER, REAL),
        pair(REAL, "short"),
    ] {
        let result = store.register_pairs(vec![numbered_pair(9), input]);
        assert!(result.is_err());
        let text = result.unwrap_err().to_string();
        assert!(!text.contains(REAL) && !text.contains(FAKE) && !text.contains(OTHER));
        assert_eq!(store_text(&fixture), before);
        assert_eq!(store.list(), listed);
    }
    let empty = fixture.root.path().join("empty");
    let mut fresh = CredentialStore::create(&empty).unwrap();
    assert!(
        fresh
            .register_pairs(vec![pair(REAL, FAKE), pair(FAKE, OTHER)])
            .is_err()
    );
    assert!(
        fresh
            .register_pairs(vec![pair(FAKE, OTHER), pair(REAL, FAKE)])
            .is_err()
    );
    assert!(fresh.list().is_empty());
    assert!(!empty.join(STORE_FILE_NAME).exists());
    store.remove(&ids[0]).unwrap();
    let retired = store_text(&fixture);
    for input in [pair(REAL, FAKE), pair(OTHER, FAKE), pair(FAKE, OTHER)] {
        assert!(store.register_pairs(vec![input]).is_err());
        assert_eq!(store_text(&fixture), retired);
    }
    assert!(!retired.contains(REAL));
    // A new fake for the old real is a new pair, not resurrection of the old fake.
    assert_ne!(
        store.register_pairs(vec![pair(REAL, OTHER)]).unwrap()[0],
        ids[0]
    );
}

#[test]
fn shared_membership_moves_atomically_and_last_scope_permanently_retires_only_its_pair() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_pairs(vec![pair(REAL, FAKE), numbered_pair(3)])
        .unwrap();
    let pair_id = &ids[0];
    store
        .sync_scopes(&[
            (scope("a"), vec![pair_id.clone()]),
            (scope("b"), vec![pair_id.clone()]),
        ])
        .unwrap();
    assert_eq!(store.fake(pair_id, &scope("a")), Some(FAKE));
    assert_eq!(store.fake(pair_id, &scope("b")), Some(FAKE));
    store
        .sync_scopes(&[(scope("b"), vec![pair_id.clone()])])
        .unwrap();
    assert_eq!(store.fake(pair_id, &scope("a")), None);
    assert_eq!(store.fake(pair_id, &scope("b")), Some(FAKE));
    store
        .sync_scopes(&[(scope("c"), vec![pair_id.clone(), pair_id.clone()])])
        .unwrap();
    assert_eq!(store.fake(pair_id, &scope("b")), None);
    assert_eq!(store.fake(pair_id, &scope("c")), Some(FAKE));
    let before = store_text(&fixture);
    store
        .sync_scopes(&[(scope("c"), vec![pair_id.clone()])])
        .unwrap();
    assert_eq!(store_text(&fixture), before);
    store.sync_scopes(&[]).unwrap();
    assert_eq!(store.fake(pair_id, &scope("c")), None);
    assert!(!store_text(&fixture).contains(REAL));
    let listed = store.list();
    let retired = listed
        .iter()
        .find(|summary| summary.id == *pair_id)
        .unwrap();
    assert_eq!(retired.state, CredentialState::Retired);
    assert_eq!(retired.fakes.len(), 1);
    assert_eq!(retired.fakes[0].scope_id, scope("c"));
    assert!(retired.fakes[0].retired);
    assert_eq!(
        listed
            .iter()
            .find(|summary| summary.id == ids[1])
            .unwrap()
            .state,
        CredentialState::Active
    );
    let persisted = store_text(&fixture);
    assert!(matches!(
        store.sync_scopes(&[(scope("c"), vec![pair_id.clone()])]),
        Err(CredentialStoreError::Retired(_))
    ));
    store.remove(pair_id).unwrap();
    store.sync_scopes(&[]).unwrap();
    assert_eq!(store_text(&fixture), persisted);
    drop(store);
    let mut reopened = CredentialStore::open(&fixture.store_dir).unwrap();
    assert_eq!(reopened.list(), listed);
    assert!(matches!(
        reopened.register_pairs(vec![pair(REAL, FAKE)]),
        Err(CredentialStoreError::Retired(_))
    ));
    assert!(matches!(
        reopened.sync_scopes(&[(scope("a"), vec![pair_id.clone()])]),
        Err(CredentialStoreError::Retired(_))
    ));
}

#[test]
fn same_real_different_fakes_are_allowed_only_in_distinct_scopes() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_pairs(vec![pair(REAL, FAKE), pair(REAL, OTHER)])
        .unwrap();
    store
        .sync_scopes(&[
            (scope("a"), vec![ids[0].clone()]),
            (scope("b"), vec![ids[1].clone()]),
        ])
        .unwrap();
    let before = store_text(&fixture);
    let listed = store.list();
    assert!(matches!(
        store.validate_scope_pairs(&ids),
        Err(CredentialStoreError::Conflict(_))
    ));
    store.validate_scope_pairs(&ids[..1]).unwrap();
    assert_eq!(store_text(&fixture), before);
    for assignments in [
        vec![(scope("a"), ids.clone())],
        vec![
            (scope("a"), vec![ids[0].clone()]),
            (scope("a"), vec![ids[1].clone()]),
        ],
        vec![(scope("b"), vec![id("missing")])],
    ] {
        assert!(store.sync_scopes(&assignments).is_err());
        assert_eq!(store_text(&fixture), before);
        assert_eq!(store.list(), listed);
    }
    store.remove(&ids[0]).unwrap();
    assert_eq!(store.fake(&ids[0], &scope("a")), None);
    assert_eq!(store.fake(&ids[1], &scope("b")), Some(OTHER));
    let before = store_text(&fixture);
    store.remove(&ids[0]).unwrap();
    assert_eq!(store_text(&fixture), before);
    assert!(matches!(
        store.remove(&id("missing")),
        Err(CredentialStoreError::UnknownCredential(_))
    ));
}

#[test]
fn per_scope_capacity_counts_unique_pairs_and_rejects_the_entire_sync() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_pairs(
            (0..=MAX_ACTIVE_FAKES_PER_SCOPE)
                .map(numbered_pair)
                .collect(),
        )
        .unwrap();
    let before = store_text(&fixture);
    assert!(matches!(
        store.sync_scopes(&[(scope("a"), ids.clone())]),
        Err(CredentialStoreError::Capacity(_))
    ));
    assert_eq!(store_text(&fixture), before);
    assert!(store.list().iter().all(|summary| summary.fakes.is_empty()));
    let mut first = ids[..MAX_ACTIVE_FAKES_PER_SCOPE].to_vec();
    first.push(ids[0].clone());
    store
        .sync_scopes(&[
            (scope("a"), first),
            (scope("b"), vec![ids[MAX_ACTIVE_FAKES_PER_SCOPE].clone()]),
        ])
        .unwrap();
    assert!(store.fake(&ids[0], &scope("a")).is_some());
    assert!(
        store
            .fake(&ids[MAX_ACTIVE_FAKES_PER_SCOPE], &scope("b"))
            .is_some()
    );
}

#[test]
fn permanent_tombstones_consume_global_capacity() {
    let fixture = fixture();
    drop(CredentialStore::create(&fixture.store_dir).unwrap());
    let credentials: Vec<_> = (0..MAX_FAKES_TOTAL)
        .map(|index| {
            serde_json::json!({
                "id": format!("pair-{index:032x}"), "real": null,
                "fake": format!("tombstone{index:019}"), "scopeIds": [], "retiredScopeIds": []
            })
        })
        .collect();
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"version": 2, "credentials": credentials})).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut store = CredentialStore::open(&fixture.store_dir).unwrap();
    let before = store_text(&fixture);
    assert!(matches!(
        store.register_pairs(vec![pair(REAL, FAKE)]),
        Err(CredentialStoreError::Capacity(_))
    ));
    assert_eq!(store.list().len(), MAX_FAKES_TOTAL);
    assert_eq!(store_text(&fixture), before);
}

#[test]
fn materialization_has_one_entry_per_shared_fake_all_memberships_and_all_tombstones() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_pairs(vec![
            pair(REAL, FAKE),
            numbered_pair(1),
            numbered_pair(2),
            numbered_pair(3),
        ])
        .unwrap();
    store
        .sync_scopes(&[
            (
                scope("a"),
                vec![ids[0].clone(), ids[1].clone(), ids[2].clone()],
            ),
            (scope("b"), vec![ids[0].clone()]),
        ])
        .unwrap();
    let tombstone = store.fake(&ids[2], &scope("a")).unwrap().to_owned();
    store.remove(&ids[2]).unwrap();
    let target = target(&fixture);
    let unavailable = |indices: &[usize]| {
        let mut entries: Vec<_> = indices
            .iter()
            .map(|&index| {
                serde_json::json!({"credential": ids[index], "fake":
                if index == 0 { FAKE.to_owned() } else { numbered_pair(index).fake }})
            })
            .collect();
        entries.sort_by_key(|entry| entry["credential"].as_str().unwrap().to_owned());
        entries
    };
    store
        .materialize(&target, &[ids[0].clone(), ids[3].clone()], 7)
        .unwrap();
    assert_eq!(
        snapshot(&target),
        serde_json::json!({
            "version": 2, "generation": 7,
            "entries": [{"fake": FAKE, "credential": ids[0], "scopeIds": ["a", "b"], "real": REAL}],
            "retired": [{"fake": tombstone, "credential": ids[2]}],
            "unavailable": unavailable(&[1, 3])
        })
    );
    assert_eq!(mode(&target.directory.join(&target.file_name)), 0o640);
    assert_eq!(fs::read_dir(&target.directory).unwrap().count(), 1);
    store.materialize(&target, &[], 8).unwrap();
    assert_eq!(
        snapshot(&target),
        serde_json::json!({
            "version": 2, "generation": 8, "entries": [],
            "retired": [{"fake": tombstone, "credential": ids[2]}],
            "unavailable": unavailable(&[0, 1, 3])
        })
    );
    let before = snapshot(&target);
    for selected in [
        vec![ids[2].clone()],
        vec![id("missing")],
        vec![ids[0].clone(), ids[0].clone()],
    ] {
        assert!(store.materialize(&target, &selected, 9).is_err());
        assert_eq!(snapshot(&target), before);
    }
    let group = fs::metadata(&target.directory).unwrap().gid();
    let with_group = SecretsTarget {
        group: Some(group),
        ..target.clone()
    };
    store
        .materialize(&with_group, std::slice::from_ref(&ids[0]), 10)
        .unwrap();
    assert_eq!(
        fs::metadata(target.directory.join(&target.file_name))
            .unwrap()
            .gid(),
        group
    );
    for bad in [
        SecretsTarget {
            file_name: "../escape".into(),
            ..target.clone()
        },
        SecretsTarget {
            file_name: String::new(),
            ..target.clone()
        },
        SecretsTarget {
            directory: fixture.store_dir.clone(),
            file_name: STORE_FILE_NAME.into(),
            group: None,
        },
    ] {
        assert!(
            store
                .materialize(&bad, std::slice::from_ref(&ids[0]), 11)
                .is_err()
        );
    }
}

#[test]
fn values_do_not_leak_in_debug_summaries_or_parse_errors_even_for_short_pairs() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let input = pair(REAL, FAKE);
    let debug = format!("{input:?}");
    assert!(!debug.contains(REAL) && !debug.contains(FAKE));
    let ids = store.register_pairs(vec![input, pair("xy", "Z!")]).unwrap();
    store.sync_scopes(&[(scope("dev"), ids.clone())]).unwrap();
    let listed = serde_json::to_string(&store.list()).unwrap();
    let debug = format!("{store:?}");
    for text in [&listed, &debug] {
        assert!(!text.contains(REAL) && !text.contains(FAKE) && !text.contains("Z!"));
    }
    assert!(
        store
            .list()
            .iter()
            .find(|summary| summary.id == ids[1])
            .unwrap()
            .fakes[0]
            .suffix
            .is_empty()
    );
    drop(store);
    fs::write(
        fixture.store_dir.join(STORE_FILE_NAME),
        format!("{{\"version\": \"{REAL}\"}}"),
    )
    .unwrap();
    let error = CredentialStore::open(&fixture.store_dir).unwrap_err();
    assert!(!error.to_string().contains(REAL));
}

#[test]
fn version_one_is_explicitly_rejected_without_modifying_any_user_file() {
    let fixture = fixture();
    drop(CredentialStore::create(&fixture.store_dir).unwrap());
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    let legacy = serde_json::json!({"version": 1, "credentials": [{"real": REAL, "carriers": ["header_token"]}]}).to_string();
    fs::write(&path, &legacy).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let extra = fixture.store_dir.join(".tmp-user-file");
    fs::write(&extra, "keep").unwrap();
    for result in [
        CredentialStore::open(&fixture.store_dir),
        CredentialStore::create(&fixture.store_dir),
    ] {
        let error = result.unwrap_err();
        assert!(matches!(error, CredentialStoreError::Corrupt(_)));
        assert!(error.to_string().contains("unsupported store version 1"));
        assert_eq!(fs::read_to_string(&path).unwrap(), legacy);
        assert_eq!(fs::read_to_string(&extra).unwrap(), "keep");
    }
}

#[test]
fn persisted_registration_corruption_is_rejected_without_modifying_or_echoing_input() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_policy_pairs(SOURCE_KEY, vec![pair(REAL, FAKE)])
        .unwrap();
    let original: serde_json::Value = serde_json::from_str(&store_text(&fixture)).unwrap();
    drop(store);
    let mut registrations = Vec::new();
    for key in [
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        registrations.push(serde_json::json!({key: ids}));
    }
    registrations.extend([
        serde_json::json!({SOURCE_KEY: []}),
        serde_json::json!({SOURCE_KEY: vec![ids[0].clone(); 257]}),
        serde_json::json!({SOURCE_KEY: ["missing"]}),
        serde_json::json!({SOURCE_KEY: ["Bad/Id"]}),
        serde_json::json!({SOURCE_KEY: {"source": REAL, "realTokenFile": FAKE, "pairIds": ids}}),
    ]);
    let excessive: serde_json::Map<String, serde_json::Value> = (0..4097)
        .map(|index| (format!("{index:064x}"), serde_json::json!(ids)))
        .collect();
    registrations.push(excessive.into());
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    for registration in registrations {
        let mut variant = original.clone();
        variant["registrations"] = registration;
        let bytes = serde_json::to_vec(&variant).unwrap();
        fs::write(&path, &bytes).unwrap();
        let error = CredentialStore::open(&fixture.store_dir).unwrap_err();
        assert!(matches!(error, CredentialStoreError::Corrupt(_)));
        assert!(!error.to_string().contains(REAL) && !error.to_string().contains(FAKE));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn persisted_pair_and_membership_corruption_is_rejected_without_echoing_values() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_pairs(vec![pair(REAL, FAKE), pair(REAL, OTHER)])
        .unwrap();
    store
        .sync_scopes(&[
            (scope("a"), vec![ids[0].clone()]),
            (scope("b"), vec![ids[1].clone()]),
        ])
        .unwrap();
    let original: serde_json::Value = serde_json::from_str(&store_text(&fixture)).unwrap();
    drop(store);
    let mut variants = Vec::new();
    let mut duplicate = original.clone();
    duplicate["credentials"][1]["fake"] = duplicate["credentials"][0]["fake"].clone();
    variants.push(duplicate);
    let mut ambiguous = original.clone();
    ambiguous["credentials"][1]["scopeIds"] = ambiguous["credentials"][0]["scopeIds"].clone();
    variants.push(ambiguous);
    let mut similar = original.clone();
    similar["credentials"][0]["fake"] = similar["credentials"][0]["real"].clone();
    variants.push(similar);
    let mut retired = original.clone();
    retired["credentials"][0]["real"] = serde_json::Value::Null;
    variants.push(retired);
    let mut collision = original.clone();
    collision["credentials"][1]["real"] = collision["credentials"][0]["fake"].clone();
    variants.push(collision);
    let mut metadata = original;
    metadata["credentials"][0]["carriers"] = serde_json::json!(["header_token"]);
    variants.push(metadata);
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    for variant in variants {
        let bytes = serde_json::to_vec(&variant).unwrap();
        fs::write(&path, &bytes).unwrap();
        let error = CredentialStore::open(&fixture.store_dir).unwrap_err();
        assert!(matches!(error, CredentialStoreError::Corrupt(_)));
        assert!(!error.to_string().contains(REAL) && !error.to_string().contains(FAKE));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn store_path_permissions_links_and_missing_parents_are_refused() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    store.register_pairs(vec![pair(REAL, FAKE)]).unwrap();
    drop(store);
    fs::set_permissions(&fixture.store_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(CredentialStore::open(&fixture.store_dir).is_err());
    fs::set_permissions(&fixture.store_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(CredentialStore::open(&fixture.store_dir).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = fixture.root.path().join("alias");
    symlink(&fixture.store_dir, &alias).unwrap();
    assert!(CredentialStore::open(&alias).is_err());
    assert!(CredentialStore::create(&alias.join("nested")).is_err());
    let copy = fixture.root.path().join("copy");
    fs::hard_link(&path, &copy).unwrap();
    assert!(CredentialStore::open(&fixture.store_dir).is_err());
    fs::remove_file(&copy).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&copy, &path).unwrap();
    assert!(CredentialStore::open(&fixture.store_dir).is_err());
    let missing = fixture.root.path().join("missing");
    assert!(CredentialStore::create(&missing.join("credentials")).is_err());
    assert!(!missing.exists());
}

#[test]
fn failed_commit_does_not_publish_registration_scope_changes_or_retirement() {
    let fixture = fixture();
    let mut store = CredentialStore::create(&fixture.store_dir).unwrap();
    let ids = store
        .register_policy_pairs(SOURCE_KEY, vec![pair(REAL, FAKE)])
        .unwrap();
    store.sync_scopes(&[(scope("a"), ids.clone())]).unwrap();
    let before = store.list();
    let persisted = store_text(&fixture);
    let new_key = "b".repeat(64);
    // Renaming onto a non-empty directory fails even for root.
    let path = fixture.store_dir.join(STORE_FILE_NAME);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("block"), "x").unwrap();
    assert!(store.register_pairs(vec![numbered_pair(9)]).is_err());
    assert!(
        store
            .register_policy_pairs(&new_key, vec![numbered_pair(9)])
            .is_err()
    );
    assert!(
        store
            .register_policy_pairs(&new_key, vec![pair(REAL, FAKE)])
            .is_err()
    );
    assert_eq!(store.lookup_registration(&new_key).unwrap(), None);
    assert_eq!(
        store.lookup_registration(SOURCE_KEY).unwrap(),
        Some(ids.clone())
    );
    assert!(store.sync_scopes(&[]).is_err());
    assert!(store.sync_scopes(&[(scope("b"), ids.clone())]).is_err());
    assert!(store.remove(&ids[0]).is_err());
    assert_eq!(store.list(), before);
    assert_eq!(store.fake(&ids[0], &scope("a")), Some(FAKE));
    assert_eq!(store.fake(&ids[0], &scope("b")), None);
    assert!(!fs::read_dir(&fixture.store_dir).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp-")
    }));
    fs::remove_file(path.join("block")).unwrap();
    fs::remove_dir(&path).unwrap();
    fs::write(&path, &persisted).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    drop(store);
    let reopened = CredentialStore::open(&fixture.store_dir).unwrap();
    assert_eq!(reopened.list(), before);
    assert_eq!(reopened.lookup_registration(SOURCE_KEY).unwrap(), Some(ids));
    assert_eq!(reopened.lookup_registration(&new_key).unwrap(), None);
    assert_eq!(store_text(&fixture), persisted);
}

#[test]
fn valid_store_open_cleans_crashed_writer_temporaries() {
    let fixture = fixture();
    drop(CredentialStore::create(&fixture.store_dir).unwrap());
    let leftover = fixture.store_dir.join(".tmp-0011");
    fs::write(&leftover, REAL).unwrap();
    drop(CredentialStore::open(&fixture.store_dir).unwrap());
    assert!(!leftover.exists());
}

// /tmp is intentionally rejected by the CLI helpers. Use a private fixture below
// an already trusted workspace, or report the exact environment limitation.
fn token_fixture() -> Option<tempfile::TempDir> {
    let base = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    for ancestor in base.ancestors() {
        let metadata = fs::metadata(ancestor).unwrap();
        if (metadata.uid() != 0 && metadata.uid() != uid) || metadata.mode() & 0o7022 != 0 {
            eprintln!(
                "token helper test skipped: workspace ancestor is not trusted: {}",
                ancestor.display()
            );
            return None;
        }
    }
    Some(tempfile::tempdir_in(base).unwrap())
}

#[test]
fn token_file_reads_private_bounded_values_trims_one_line_ending_and_never_echoes_errors() {
    let Some(root) = token_fixture() else { return };
    let path = root.path().join("token");
    let uid = rustix::process::geteuid().as_raw();
    for ending in ["", "\n", "\r", "\r\n"] {
        fs::write(&path, format!("{REAL}{ending}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token_file(&path, uid).unwrap().expose(), REAL);
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(read_token_file(&path, uid).unwrap().expose(), REAL);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, format!("{}\r\n", "a".repeat(512))).unwrap();
    assert_eq!(
        read_token_file(&path, uid).unwrap().expose(),
        "a".repeat(512)
    );
    for value in [
        String::new(),
        "a".repeat(513),
        "a".repeat(515),
        format!("{REAL}\n\n"),
        format!("{REAL} "),
        format!("{REAL}\t"),
        format!("{REAL}\ninternal"),
        "é".into(),
    ] {
        fs::write(&path, &value).unwrap();
        let error = read_token_file(&path, uid).unwrap_err().to_string();
        assert!(value.is_empty() || !error.contains(&value));
        assert!(!error.contains(REAL));
    }
    fs::write(&path, [0xff_u8]).unwrap();
    assert!(read_token_file(&path, uid).is_err());
    assert!(read_token_file(&path, uid.wrapping_add(1)).is_err());
    assert!(read_token_file(&root.path().join("missing"), uid).is_err());
}

#[test]
fn token_helpers_refuse_untrusted_paths_permissions_links_and_nonregular_files() {
    let Some(root) = token_fixture() else { return };
    let path = root.path().join("token");
    let uid = rustix::process::geteuid().as_raw();
    write_fake_file(&path, FAKE).unwrap();
    for permissions in [0o640, 0o604, 0o620, 0o602, 0o4600] {
        fs::set_permissions(&path, fs::Permissions::from_mode(permissions)).unwrap();
        assert!(read_token_file(&path, uid).is_err());
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    for permissions in [0o720, 0o702] {
        fs::set_permissions(root.path(), fs::Permissions::from_mode(permissions)).unwrap();
        assert!(read_token_file(&path, uid).is_err());
        assert!(write_fake_file(&root.path().join("new"), FAKE).is_err());
        assert!(!root.path().join("new").exists());
    }
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let link = root.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(read_token_file(&link, uid).is_err());
    assert!(write_fake_file(&link, OTHER).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), FAKE);
    let alias = root.path().join("alias");
    symlink(root.path(), &alias).unwrap();
    assert!(read_token_file(&alias.join("token"), uid).is_err());
    assert!(write_fake_file(&alias.join("new"), FAKE).is_err());
    let hard = root.path().join("hard");
    fs::hard_link(&path, &hard).unwrap();
    assert!(read_token_file(&path, uid).is_err());
    fs::remove_file(&hard).unwrap();
    assert!(read_token_file(root.path(), uid).is_err());
    for bad in [
        PathBuf::from("relative/token"),
        root.path().join("../token"),
        PathBuf::from(format!("{}/./token", root.path().display())),
        PathBuf::from(format!("{}//token", root.path().display())),
    ] {
        assert!(read_token_file(&bad, uid).is_err());
        assert!(write_fake_file(&bad, FAKE).is_err());
    }
}

#[test]
fn fake_file_is_create_new_private_and_validation_or_generation_failure_creates_nothing() {
    let Some(root) = token_fixture() else { return };
    let path = root.path().join("fake");
    write_fake_file(&path, FAKE).unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(fs::metadata(&path).unwrap().nlink(), 1);
    assert_eq!(fs::read_to_string(&path).unwrap(), FAKE);
    assert!(write_fake_file(&path, OTHER).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), FAKE);
    let absent = root.path().join("absent");
    assert!(write_fake_file(&absent, "has space").is_err());
    assert!(!absent.exists());
    assert!(
        generate_fake("unknown-structured-token", "auto")
            .and_then(|fake| write_fake_file(&absent, &fake))
            .is_err()
    );
    assert!(!absent.exists());
}
