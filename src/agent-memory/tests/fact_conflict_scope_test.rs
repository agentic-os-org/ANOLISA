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
    let conflicts = store.detect_conflicts("walrus walrus", -100.0).unwrap();
    assert_eq!(
        conflicts.len(),
        5,
        "all five indexed records must be candidates"
    );
    let index = Arc::new(Mutex::new(store));
    let new = fact();
    FactWriter::new(tmp.path())
        .with_index(index.clone(), -100.0)
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
