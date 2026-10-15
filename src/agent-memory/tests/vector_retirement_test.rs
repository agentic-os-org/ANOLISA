//! Retired facts cannot re-enter vector or fused search rankings.

use agent_memory::index::store::BM25Store;

fn store(root: &std::path::Path) -> BM25Store {
    let dir = root.join(".anolisa/index");
    std::fs::create_dir_all(&dir).unwrap();
    BM25Store::open(&dir.join("bm25.db"), 0.0, 0.0, true).unwrap()
}

#[test]
fn vector_filters_retired_rows_before_top_k_and_keeps_unindexed_vectors() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = store(tmp.path());
    s.upsert("facts/old.md", 1, 10, "needle archived", None)
        .unwrap();
    s.upsert("facts/live.md", 1, 10, "needle current", None)
        .unwrap();
    s.upsert_vec("facts/old.md", &[1.0, 0.0]).unwrap();
    s.upsert_vec("facts/live.md", &[0.8, 0.6]).unwrap();
    s.upsert_vec("legacy.md", &[0.6, 0.8]).unwrap();
    assert_eq!(s.search_vec(&[1.0, 0.0], 1).unwrap()[0].0, "facts/old.md");

    s.supersede("facts/old.md", "replacement").unwrap();
    let hits = s.search_vec(&[1.0, 0.0], 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "facts/live.md");
    let paths: Vec<_> = s
        .search_vec(&[1.0, 0.0], 10)
        .unwrap()
        .into_iter()
        .map(|h| h.0)
        .collect();
    assert_eq!(paths, ["facts/live.md", "legacy.md"]);
    assert!(s.search_vec(&[1.0, 0.0], 0).unwrap().is_empty());
}

#[test]
fn hybrid_never_resurrects_retired_facts_through_the_vector_leg() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = store(tmp.path());
    for path in ["facts/old.md", "facts/live.md"] {
        s.upsert(path, 1, 10, "needle memory", None).unwrap();
        s.upsert_vec(path, &[1.0, 0.0]).unwrap();
    }
    assert_eq!(s.search_hybrid("needle", &[1.0, 0.0], 10).unwrap().len(), 2);
    s.supersede("facts/old.md", "replacement").unwrap();
    for exclude_cold in [true, false] {
        let hits = s
            .search_hybrid_with_cold("needle", &[1.0, 0.0], 1, exclude_cold)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "facts/live.md");
    }
    s.supersede("facts/live.md", "replacement").unwrap();
    assert!(s.search_vec(&[1.0, 0.0], 10).unwrap().is_empty());
    assert!(
        s.search_hybrid("needle", &[1.0, 0.0], 10)
            .unwrap()
            .is_empty()
    );
}
