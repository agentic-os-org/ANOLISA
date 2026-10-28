//! Fact-category restrictions must precede vector and hybrid candidate limits.

use agent_memory::index::BM25Store;

fn store(tmp: &tempfile::TempDir) -> BM25Store {
    let index_dir = tmp.path().join(".anolisa/index");
    std::fs::create_dir_all(&index_dir).unwrap();
    BM25Store::open(&index_dir.join("bm25.db"), 0.0, 0.0, true).unwrap()
}

fn insert(store: &mut BM25Store, path: &str, body: &str, vector: &[f32]) {
    store
        .upsert(path, 0, body.len() as u64, body, None)
        .unwrap();
    store.upsert_vec(path, vector).unwrap();
}

fn paths(store: &BM25Store, query: &str, category: &str) -> Vec<Vec<String>> {
    vec![
        store
            .search_vec_in_category(&[1.0, 0.0], 2, Some(category))
            .unwrap()
            .into_iter()
            .map(|(path, _)| path)
            .collect(),
        store
            .search_hybrid_in_category(query, &[1.0, 0.0], 2, Some(category))
            .unwrap()
            .into_iter()
            .map(|hit| hit.path)
            .collect(),
    ]
}

#[test]
fn hybrid_category_filters_both_candidate_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = store(&tmp);
    for i in 0..6 {
        insert(
            &mut store,
            &format!("facts/lesson/{i}.md"),
            "marker",
            &[1.0, 0.0],
        );
    }
    for i in 0..2 {
        insert(
            &mut store,
            &format!("facts/interest/{i}.md"),
            "marker additional context",
            &[0.8, 0.6],
        );
    }
    // Both unfiltered candidate windows exclude every requested-category fact.
    let bm25 = store.search("marker", 4, true).unwrap();
    let vectors = store.search_vec(&[1.0, 0.0], 4).unwrap();
    assert_eq!(bm25.len(), 4);
    assert_eq!(vectors.len(), 4);
    assert!(bm25.iter().all(|h| h.path.starts_with("facts/lesson/")));
    assert!(vectors.iter().all(|(p, _)| p.starts_with("facts/lesson/")));

    for query in ["marker", "unmatched"] {
        for hits in paths(&store, query, "interest") {
            assert_eq!(hits.len(), 2, "query={query}: {hits:?}");
            assert!(hits.iter().all(|p| p.starts_with("facts/interest/")));
        }
    }
}

#[test]
fn embedding_categories_are_literal_case_sensitive_fact_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = store(&tmp);
    for path in [
        "facts/interest/a.md",
        "facts/Interest/a.md",
        "notes/interest/a.md",
        "facts/a*b?[c/a.md",
        "facts/axbyc/a.md",
    ] {
        insert(&mut store, path, "marker", &[1.0, 0.0]);
    }
    for (category, expected) in [
        ("interest", vec!["facts/interest/a.md".to_string()]),
        ("Interest", vec!["facts/Interest/a.md".to_string()]),
        ("a*b?[c", vec!["facts/a*b?[c/a.md".to_string()]),
        ("missing", vec![]),
    ] {
        for hits in paths(&store, "marker", category) {
            assert_eq!(hits, expected, "category={category}");
        }
    }
}

#[test]
fn category_filter_keeps_standalone_vectors_and_lifecycle_exclusions() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = store(&tmp);
    let cold = "facts/interest/cold.md";
    insert(&mut store, cold, "marker", &[1.0, 0.0]);
    assert_eq!(store.compact(1).unwrap(), 1);
    let old = "facts/interest/old.md";
    insert(&mut store, old, "marker", &[1.0, 0.0]);
    store.supersede(old, "replacement").unwrap();
    let current = "facts/interest/current.md";
    insert(&mut store, current, "marker", &[0.8, 0.6]);
    let standalone = "facts/interest/standalone.md";
    store.upsert_vec(standalone, &[1.0, 0.0]).unwrap();

    for mut hits in paths(&store, "marker", "interest") {
        hits.sort();
        assert_eq!(hits, vec![current.to_string(), standalone.to_string()]);
    }
    assert_eq!(store.search_vec(&[1.0, 0.0], 10).unwrap().len(), 2);
    assert_eq!(
        store
            .search_hybrid("marker", &[1.0, 0.0], 10)
            .unwrap()
            .len(),
        2
    );
}
