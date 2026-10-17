//! Fact consolidation only retires Markdown fact records, never their journal.

use std::sync::{Arc, Mutex};

use agent_memory::consolidation::fact::{ConsolidatedFact, FactCategory};
use agent_memory::consolidation::writer::FactWriter;
use agent_memory::index::store::BM25Store;

fn fact() -> ConsolidatedFact {
    ConsolidatedFact::new(
        "fixture",
        FactCategory::WorkingContext,
        "walrus".into(),
        "walrus".into(),
        "memory_search".into(),
        vec![],
        0.9,
    )
}

#[test]
fn consolidation_retires_the_conflicting_fact_but_preserves_journals_and_notes() {
    let tmp = tempfile::tempdir().unwrap();
    let old = fact();
    FactWriter::new(tmp.path()).write(&old).unwrap();
    let old_path = format!("facts/working-context/{}.md", old.id);
    let retained = [
        "facts/facts.jsonl",
        "notes/source.md",
        "facts/extra.jsonl",
        "facts-other/decoy.md",
    ];
    // The first write already created facts/facts.jsonl.
    for path in &retained[1..] {
        let full = tmp.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, "walrus").unwrap();
    }
    let dir = tmp.path().join(".anolisa/index");
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = BM25Store::open(&dir.join("bm25.db"), 0.0, 0.0, true).unwrap();
    for path in std::iter::once(old_path.as_str()).chain(retained.iter().copied()) {
        let body = std::fs::read_to_string(tmp.path().join(path)).unwrap();
        store
            .upsert(path, 1, body.len() as u64, &body, None)
            .unwrap();
    }
    let conflicts = store.detect_conflicts("walrus walrus", 0.0).unwrap();
    let candidates: std::collections::BTreeSet<_> =
        conflicts.iter().map(|(path, _)| path.as_str()).collect();
    let expected: std::collections::BTreeSet<_> = std::iter::once(old_path.as_str())
        .chain(retained.iter().copied())
        .collect();
    assert_eq!(
        candidates, expected,
        "fixture must include every record type"
    );
    let index = Arc::new(Mutex::new(store));
    let new = fact();
    FactWriter::new(tmp.path())
        .with_index(index.clone(), 0.0)
        .write(&new)
        .unwrap();

    let hits = index.lock().unwrap().search("walrus", 20, true).unwrap();
    let paths: Vec<_> = hits.iter().map(|h| h.path.as_str()).collect();
    assert!(!paths.contains(&old_path.as_str()));
    for path in retained {
        assert!(paths.contains(&path), "non-fact record was retired: {path}");
    }
    let old_body = std::fs::read_to_string(tmp.path().join(old_path)).unwrap();
    assert!(old_body.contains(&format!("superseded_by: {}", new.id)));
    let journal = std::fs::read_to_string(tmp.path().join("facts/facts.jsonl")).unwrap();
    let entries: Vec<ConsolidatedFact> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, old.id);
    assert_eq!(entries[1].id, new.id);
    for path in &retained[1..] {
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(path)).unwrap(),
            "walrus"
        );
    }
}

#[test]
fn non_facts_cannot_exhaust_fact_conflict_candidates() {
    for (term, count) in [("walrus", 6), ("海狸", 55)] {
        let tmp = tempfile::tempdir().unwrap();
        let mut old = fact();
        old.category = FactCategory::Lesson;
        old.title = term.into();
        old.content = format!("{term} {}", "padding ".repeat(100));
        FactWriter::new(tmp.path()).write(&old).unwrap();
        let old_path = format!("facts/lesson/{}.md", old.id);
        let dir = tmp.path().join(".anolisa/index");
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = BM25Store::open(&dir.join("bm25.db"), 0.0, 0.0, true).unwrap();
        // Insert more non-facts than either query path's candidate budget.
        let mut retained = Vec::new();
        for i in 0..count {
            let path = format!("notes/journal{i}.jsonl");
            store
                .upsert(&path, 1, 10, &format!("{term} {term} {term}"), None)
                .unwrap();
            retained.push(path);
        }
        let body = std::fs::read_to_string(tmp.path().join(&old_path)).unwrap();
        store
            .upsert(&old_path, 1, body.len() as u64, &body, None)
            .unwrap();
        let query = format!("{term} {term}");
        assert!(
            store
                .detect_conflicts(&query, 0.0)
                .unwrap()
                .iter()
                .all(|(p, _)| p != &old_path),
            "fixture must crowd the fact out of the general query"
        );
        let index = Arc::new(Mutex::new(store));
        let mut new = fact();
        new.title = term.into();
        new.content = term.into();
        FactWriter::new(tmp.path())
            .with_index(index.clone(), 0.0)
            .write(&new)
            .unwrap();
        let hits = index.lock().unwrap().search(term, 100, true).unwrap();
        assert!(
            !hits.iter().any(|h| h.path == old_path),
            "fact in another category must still retire ({term})"
        );
        for path in retained {
            assert!(
                hits.iter().any(|h| h.path == path),
                "non-fact must survive ({term}): {path}"
            );
        }
        assert!(
            std::fs::read_to_string(tmp.path().join(old_path))
                .unwrap()
                .contains(&format!("superseded_by: {}", new.id))
        );
    }
}

#[test]
fn default_threshold_retires_a_strong_conflicting_fact() {
    let tmp = tempfile::tempdir().unwrap();
    let old = fact();
    FactWriter::new(tmp.path()).write(&old).unwrap();
    let path = format!("facts/working-context/{}.md", old.id);
    let dir = tmp.path().join(".anolisa/index");
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = BM25Store::open(&dir.join("bm25.db"), 0.0, 0.0, true).unwrap();
    let body = std::fs::read_to_string(tmp.path().join(&path)).unwrap();
    store
        .upsert(&path, 1, body.len() as u64, &body, None)
        .unwrap();
    for i in 0..10 {
        store
            .upsert(
                &format!("notes/noise{i}.md"),
                1,
                10,
                "unrelated corpus material",
                None,
            )
            .unwrap();
    }
    let threshold = agent_memory::config::ConsolidationConfig::default().conflict_bm25_threshold;
    let hits = store.search("walrus walrus", 1, true).unwrap();
    assert_eq!(hits[0].path, path);
    assert!(
        -hits[0].score < threshold,
        "fixture must exceed the shipped BM25 strength cutoff"
    );
    println!("default raw BM25={}, cutoff={threshold}", -hits[0].score);
    let index = Arc::new(Mutex::new(store));
    let new = fact();
    FactWriter::new(tmp.path())
        .with_index(index.clone(), threshold)
        .write(&new)
        .unwrap();
    assert!(
        index
            .lock()
            .unwrap()
            .search("walrus", 20, true)
            .unwrap()
            .iter()
            .all(|h| h.path != path)
    );
    assert!(
        std::fs::read_to_string(tmp.path().join(path))
            .unwrap()
            .contains(&format!("superseded_by: {}", new.id))
    );
}

#[test]
fn default_threshold_can_miss_an_exact_fact_in_a_term_dense_corpus() {
    let tmp = tempfile::tempdir().unwrap();
    let old = fact();
    FactWriter::new(tmp.path()).write(&old).unwrap();
    let path = format!("facts/working-context/{}.md", old.id);
    let dir = tmp.path().join(".anolisa/index");
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = BM25Store::open(&dir.join("bm25.db"), 0.0, 0.0, true).unwrap();
    let body = std::fs::read_to_string(tmp.path().join(&path)).unwrap();
    store
        .upsert(&path, 1, body.len() as u64, &body, None)
        .unwrap();
    for i in 0..10 {
        store
            .upsert(
                &format!("notes/dense{i}.md"),
                1,
                10,
                &format!("walrus {}", "padding ".repeat(200)),
                None,
            )
            .unwrap();
    }
    let threshold = agent_memory::config::ConsolidationConfig::default().conflict_bm25_threshold;
    let query = format!("{} {}", old.title, old.content);
    let all = store.detect_conflicts(&query, 0.0).unwrap();
    assert!(
        all.iter()
            .any(|(p, score)| p == &path && *score > threshold)
    );
    assert!(
        store
            .detect_conflicts(&query, threshold)
            .unwrap()
            .is_empty()
    );
    let index = Arc::new(Mutex::new(store));
    let mut duplicate = old.clone();
    duplicate.id = ulid::Ulid::new().to_string();
    FactWriter::new(tmp.path())
        .with_index(index.clone(), threshold)
        .write(&duplicate)
        .unwrap();
    // Pins the documented limitation; calibration/semantics remain tracked at
    // https://github.com/agentic-os-org/ANOLISA/issues/4157.
    assert!(
        index
            .lock()
            .unwrap()
            .search("walrus", 20, true)
            .unwrap()
            .iter()
            .any(|hit| hit.path == path)
    );
    assert!(
        !std::fs::read_to_string(tmp.path().join(path))
            .unwrap()
            .contains("superseded_by:")
    );
}
