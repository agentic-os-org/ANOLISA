//! Index removal must follow literal, case-sensitive filesystem paths.

use agent_memory::index::BM25Store;

fn check_subtree_removal(removed: &str, sibling: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = BM25Store::open(&tmp.path().join("index.db"), 0.0, 0.0, false).unwrap();
    let removed_child = format!("{removed}/child.md");
    let removed_grandchild = format!("{removed}/nested/child.md");
    let retained_child = format!("{sibling}/child.md");
    for path in [&removed_child, &removed_grandchild, &retained_child] {
        store.upsert(path, 0, 6, "marker", None).unwrap();
        store.upsert_vec(path, &[1.0, 0.0]).unwrap();
    }

    assert!(store.remove(removed).unwrap());
    assert_eq!(store.known_paths().unwrap(), vec![retained_child.clone()]);
    let keyword_hits = store.search("marker", 10, false).unwrap();
    assert_eq!(keyword_hits.len(), 1);
    assert_eq!(keyword_hits[0].path, retained_child);
    assert_eq!(
        store.search_vec(&[1.0, 0.0], 10).unwrap(),
        vec![(retained_child, 1.0)]
    );
    assert!(!store.remove(removed).unwrap());
}

#[test]
fn remove_treats_underscore_as_a_literal() {
    check_subtree_removal("notes_one", "notesXone");
}

#[test]
fn remove_treats_percent_as_a_literal() {
    check_subtree_removal("notes%", "notesOther");
}

#[test]
fn remove_preserves_case_distinct_siblings() {
    check_subtree_removal("Notes", "notes");
}

#[test]
fn remove_keeps_backslashes_literal() {
    check_subtree_removal(r"notes\one", "notesone");
}

#[test]
fn remove_requires_a_directory_separator() {
    check_subtree_removal("notes", "notes-archive");
}

#[test]
fn remove_keeps_glob_characters_literal() {
    for (removed, sibling) in [
        ("notes*", "notesMore"),
        ("notes?", "notesX"),
        ("notes[ab]", "notesa"),
    ] {
        check_subtree_removal(removed, sibling);
    }
}

#[test]
fn removing_an_exact_file_cleans_up_its_keyword_and_vector_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = BM25Store::open(&tmp.path().join("index.db"), 0.0, 0.0, false).unwrap();
    for path in ["note_one.md", "noteXone.md"] {
        store.upsert(path, 0, 6, "marker", None).unwrap();
        store.upsert_vec(path, &[1.0, 0.0]).unwrap();
    }
    assert!(store.remove("note_one.md").unwrap());
    assert_eq!(store.known_paths().unwrap(), vec!["noteXone.md"]);
    assert_eq!(store.search("marker", 10, false).unwrap().len(), 1);
    assert_eq!(
        store.search_vec(&[1.0, 0.0], 10).unwrap()[0].0,
        "noteXone.md"
    );
}
