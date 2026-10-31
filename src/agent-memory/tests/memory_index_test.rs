//! Integration tests for the MEMORY.md index round trip.
//!
//! `parse_index` has to read back exactly what `IndexEntry::to_line` wrote:
//! both `update_index_entry` (upsert) and `remove_index_entry` key on the
//! parsed path. A title containing `"]("` — free-form text lifted straight
//! from frontmatter — used to break that key, so the upsert appended a
//! duplicate line on every write and removal silently missed.

use tempfile::tempdir;

use agent_memory::config::{AppConfig, Profile};
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_index::{
    IndexEntry, parse_index, refresh_index, remove_index_entry, update_index_entry,
};

const INDEX_FILE: &str = "MEMORY.md";
const NOTE_PATH: &str = "notes/brackets.md";
/// A title that embeds the link opener, so a parse anchored on the first
/// `"]("` cuts the title short and reads the rest as a garbage path.
const BRACKET_TITLE: &str = "array literal ]( vs concat";

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.profile = Profile::Advanced;
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

/// Write the note whose frontmatter title carries the bracket, then build
/// the index from it.
fn seed(svc: &MemoryService) {
    let body = format!("---\ntitle: \"{BRACKET_TITLE}\"\n---\nPick one.\n");
    svc.write(NOTE_PATH, &body, false).unwrap();
    refresh_index(svc).unwrap();
}

fn index_entries(svc: &MemoryService) -> Vec<IndexEntry> {
    let content = std::fs::read_to_string(svc.mount.root.join(INDEX_FILE)).unwrap();
    parse_index(&content)
}

/// The index also carries the store's bootstrap `README.md`, so every
/// assertion below narrows to the seeded note.
fn note_entries(svc: &MemoryService) -> Vec<IndexEntry> {
    index_entries(svc)
        .into_iter()
        .filter(|e| e.path == NOTE_PATH)
        .collect()
}

#[test]
fn refresh_index_keeps_the_real_path_for_a_bracket_title() {
    let (_tmp, svc) = setup();
    seed(&svc);

    let entries = note_entries(&svc);
    assert_eq!(entries.len(), 1, "one line for the note: {entries:?}");
    assert_eq!(entries[0].title, BRACKET_TITLE);
    assert_eq!(entries[0].description, "Pick one.");
}

#[test]
fn update_index_entry_upserts_a_bracket_title_instead_of_duplicating() {
    let (_tmp, svc) = setup();
    seed(&svc);

    let before = index_entries(&svc).len();
    let entry = IndexEntry {
        title: BRACKET_TITLE.into(),
        path: NOTE_PATH.into(),
        description: "revised".into(),
    };
    // Written twice: a path the parse corrupted never matched the upsert, so
    // each call appended another line for the same note.
    update_index_entry(&svc, &entry).unwrap();
    update_index_entry(&svc, &entry).unwrap();

    let entries = index_entries(&svc);
    assert_eq!(
        entries.len(),
        before,
        "upsert must replace the entry, not append a duplicate: {entries:?}"
    );
    let notes = note_entries(&svc);
    assert_eq!(
        notes.len(),
        1,
        "the note must stay findable by path: {notes:?}"
    );
    assert_eq!(notes[0].title, BRACKET_TITLE);
    assert_eq!(notes[0].description, "revised");
}

#[test]
fn remove_index_entry_matches_a_bracket_title() {
    let (_tmp, svc) = setup();
    seed(&svc);

    let before = index_entries(&svc).len();
    remove_index_entry(&svc, NOTE_PATH).unwrap();

    // A corrupted path never matched the removal filter, so the line stayed
    // in the file — assert on the file itself, not only on the re-parse.
    let content = std::fs::read_to_string(svc.mount.root.join(INDEX_FILE)).unwrap();
    assert!(
        !content.contains(NOTE_PATH),
        "removal by path must drop the line it wrote:\n{content}"
    );
    let entries = index_entries(&svc);
    assert_eq!(
        entries.len(),
        before - 1,
        "only the removed note may disappear: {entries:?}"
    );
    assert!(note_entries(&svc).is_empty());
}
