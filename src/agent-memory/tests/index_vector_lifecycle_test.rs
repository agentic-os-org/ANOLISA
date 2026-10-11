//! Semantic search must respect the same memory lifecycle as keyword search.

use agent_memory::index::BM25Store;

fn store(tmp: &tempfile::TempDir, exclude_cold: bool) -> BM25Store {
    let mut store = BM25Store::open(&tmp.path().join("index.db"), 0.0, 0.0, exclude_cold).unwrap();
    for path in ["current.md", "superseded.md"] {
        store.upsert(path, 0, 6, "marker", None).unwrap();
        store.upsert_vec(path, &[1.0, 0.0]).unwrap();
    }
    store.supersede("superseded.md", "current").unwrap();
    store
}

#[test]
fn vector_search_excludes_superseded_memories() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp, false);
    assert_eq!(store.search("marker", 5, false).unwrap().len(), 1);
    assert_eq!(
        store.search_vec(&[1.0, 0.0], 5).unwrap(),
        vec![("current.md".to_string(), 1.0)]
    );
}

#[test]
fn hybrid_search_cannot_reintroduce_superseded_memories() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp, false);
    for query in ["marker", "unmatched"] {
        let hits = store.search_hybrid(query, &[1.0, 0.0], 5).unwrap();
        let paths: Vec<_> = hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, vec!["current.md"]);
    }
}

#[test]
fn hybrid_cold_override_applies_to_both_rankings() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = store(&tmp, true);
    store.compact(1).unwrap();
    assert!(store.search("marker", 5, true).unwrap().is_empty());
    assert!(store.search_vec(&[1.0, 0.0], 5).unwrap().is_empty());
    assert!(
        store
            .search_hybrid("marker", &[1.0, 0.0], 5)
            .unwrap()
            .is_empty()
    );
    for query in ["marker", "unmatched"] {
        let hits = store
            .search_hybrid_with_cold(query, &[1.0, 0.0], 5, false)
            .unwrap();
        let paths: Vec<_> = hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, vec!["current.md"]);
    }
}

#[test]
fn hybrid_can_exclude_cold_with_an_inclusive_store_default() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = store(&tmp, false);
    store.compact(1).unwrap();
    assert!(
        store
            .search_hybrid_with_cold("unmatched", &[1.0, 0.0], 5, true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn excluded_vectors_do_not_consume_the_top_k_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = store(&tmp, false);
    store.upsert_vec("current.md", &[0.6, 0.8]).unwrap();
    assert_eq!(store.search_vec(&[1.0, 0.0], 1).unwrap()[0].0, "current.md");
    let hits = store.search_hybrid("unmatched", &[1.0, 0.0], 1).unwrap();
    assert_eq!(hits[0].path, "current.md");
}

#[test]
fn cold_vectors_do_not_consume_the_top_k_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = BM25Store::open(&tmp.path().join("index.db"), 0.0, 0.0, true).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    store.upsert("warm.md", now, 6, "marker", None).unwrap();
    store.upsert_vec("warm.md", &[0.6, 0.8]).unwrap();
    store.upsert("cold.md", 0, 6, "marker", None).unwrap();
    store.upsert_vec("cold.md", &[1.0, 0.0]).unwrap();
    assert_eq!(store.compact(1).unwrap(), 1);

    assert_eq!(store.search_vec(&[1.0, 0.0], 1).unwrap()[0].0, "warm.md");
    let hits = store.search_hybrid("unmatched", &[1.0, 0.0], 1).unwrap();
    assert_eq!(hits[0].path, "warm.md");
}

#[test]
fn standalone_vector_rows_keep_their_existing_search_behavior() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = BM25Store::open(&tmp.path().join("index.db"), 0.0, 0.0, true).unwrap();
    store.upsert_vec("standalone.md", &[1.0, 0.0]).unwrap();
    assert_eq!(
        store.search_vec(&[1.0, 0.0], 1).unwrap(),
        vec![("standalone.md".to_string(), 1.0)]
    );
}
