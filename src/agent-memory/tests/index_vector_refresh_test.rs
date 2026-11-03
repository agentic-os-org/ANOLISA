//! Indexed vectors must describe the current extracted file body.

use agent_memory::index::BM25Store;

#[test]
fn changed_body_invalidates_the_old_vector_for_backfill() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("index.db");
    let mut store = BM25Store::open(&path, 0.0, 0.0, false).unwrap();
    store.upsert("note.md", 1, 7, "oldbody", None).unwrap();
    store.upsert_vec("note.md", &[1.0, 0.0]).unwrap();
    drop(store);

    let mut store = BM25Store::open(&path, 0.0, 0.0, false).unwrap();
    store.upsert("note.md", 2, 7, "newbody", None).unwrap();
    assert_eq!(store.search("newbody", 5, false).unwrap().len(), 1);
    assert_eq!(store.paths_without_vec().unwrap(), vec!["note.md"]);
    assert!(store.search_vec(&[1.0, 0.0], 5).unwrap().is_empty());

    store.upsert_vec("note.md", &[0.0, 1.0]).unwrap();
    assert!(store.paths_without_vec().unwrap().is_empty());
    assert_eq!(
        store.search_vec(&[0.0, 1.0], 5).unwrap(),
        vec![("note.md".to_string(), 1.0)]
    );
}

#[test]
fn identical_body_preserves_an_existing_vector() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = BM25Store::open(&tmp.path().join("index.db"), 0.0, 0.0, false).unwrap();
    store.upsert("note.md", 1, 7, "samebody", None).unwrap();
    store.upsert_vec("note.md", &[1.0, 0.0]).unwrap();
    store.upsert("note.md", 2, 9, "samebody", None).unwrap();
    assert!(store.paths_without_vec().unwrap().is_empty());
    assert_eq!(
        store.search_vec(&[1.0, 0.0], 5).unwrap(),
        vec![("note.md".to_string(), 1.0)]
    );
}

#[test]
fn changed_body_with_unchanged_metadata_invalidates_the_vector() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = BM25Store::open(&tmp.path().join("index.db"), 0.0, 0.0, false).unwrap();
    store.upsert("note.md", 1, 7, "oldbody", None).unwrap();
    store.upsert_vec("note.md", &[1.0, 0.0]).unwrap();
    store.upsert("note.md", 1, 7, "newbody", None).unwrap();
    assert_eq!(store.paths_without_vec().unwrap(), vec!["note.md"]);
}

#[test]
fn failed_body_update_keeps_the_previous_vector() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("index.db");
    let mut store = BM25Store::open(&path, 0.0, 0.0, false).unwrap();
    store.upsert("note.md", 1, 7, "oldbody", None).unwrap();
    store.upsert_vec("note.md", &[1.0, 0.0]).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_update BEFORE UPDATE ON files
             BEGIN SELECT RAISE(FAIL, 'reject update'); END;",
        )
        .unwrap();

    assert!(store.upsert("note.md", 2, 7, "newbody", None).is_err());
    assert_eq!(store.search("oldbody", 5, false).unwrap().len(), 1);
    assert!(store.paths_without_vec().unwrap().is_empty());
    assert_eq!(
        store.search_vec(&[1.0, 0.0], 5).unwrap(),
        vec![("note.md".to_string(), 1.0)]
    );
}
