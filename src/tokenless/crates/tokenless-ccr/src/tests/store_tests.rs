use crate::InMemoryStore;

#[test]
fn is_empty_default_trait_method() {
    let store = InMemoryStore::new();
    assert!(store.is_empty());
    let _ = store.stash("payload").unwrap();
    assert!(!store.is_empty());
}

#[test]
fn stash_error_display() {
    let e = StashError::Backend("test error".to_string());
    assert!(format!("{e}").contains("test error"));
}

#[test]
fn default_trait_creates_working_store() {
    let store = InMemoryStore::default();
    assert!(store.is_empty());
    let key = store.stash("payload").unwrap().key;
    assert!(!store.is_empty());
    let retrieved = store.retrieve(&key).unwrap();
    assert_eq!(retrieved, Some("payload".to_string()));
}

#[cfg(feature = "sqlite")]
#[test]
fn stash_error_from_rusqlite() {
    let rusqlite_err = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(1),
        Some("test error".to_string()),
    );
    let stash_err: StashError = StashError::from(rusqlite_err);
    let msg = format!("{stash_err}");
    assert!(msg.contains("test error"));
}
