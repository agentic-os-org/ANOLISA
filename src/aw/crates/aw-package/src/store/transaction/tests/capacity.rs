//! Compare admission counts with real preparation, including current registration metadata.

use super::*;

#[test]
fn action_capacity_matches_preparation_for_current_locator_changes() {
    for existing in [false, true] {
        for remove_locator in [false, true] {
            if remove_locator && !existing {
                continue;
            }
            let fixture = Fixture::new();
            let mut changes = fixture.changes(existing);
            if remove_locator {
                changes.writes.clear();
                changes.removes = changes.verify.clone();
                changes.database_after.installations[0].packages.clear();
                changes.marker_after = None;
            }
            let count = super::super::capacity::actions(&fixture.paths, &changes).unwrap();
            let expected = if remove_locator {
                2
            } else if existing {
                1
            } else {
                2
            };
            assert_eq!(count, expected);
            let prepared = prepare::begin(&fixture.paths, &changes).unwrap();
            assert_eq!(count, prepared.journal.actions.len());
            prepared
                .journal
                .validate(&fixture.paths, &prepared.journal.directory())
                .unwrap();
        }
    }
}

#[test]
fn journal_record_guards_reject_action_and_directory_overflow_before_publication() {
    let fixture = Fixture::new();
    let changes = fixture.changes(false);
    let prepared = prepare::begin(&fixture.paths, &changes).unwrap();
    let mut entry = prepared.journal.clone();
    let action = entry.actions[0].clone();
    entry.actions = vec![action; super::super::capacity::MAX_ACTIONS + 1];
    assert!(entry
        .validate(&fixture.paths, &entry.directory())
        .unwrap_err()
        .to_string()
        .contains("record limits"));
    let mut entry = prepared.journal.clone();
    let identity = fs::directory_identity(&fixture.root).unwrap();
    for index in 0..=super::super::capacity::MAX_DIRECTORIES {
        entry
            .directories
            .insert(format!("share/directory-{index}"), identity.clone());
    }
    assert!(entry
        .validate(&fixture.paths, &entry.directory())
        .unwrap_err()
        .to_string()
        .contains("record limits"));
    entry.directories.remove(&format!(
        "share/directory-{}",
        super::super::capacity::MAX_DIRECTORIES
    ));
    let before = disk::read(entry.directory().join("journal.json")).unwrap();
    fixture.directory(&fixture.prefix);
    let error = prepare::parents(&mut entry, Path::new("unexpected/leaf")).unwrap_err();
    assert!(error.to_string().contains("directory capacity"));
    assert!(!fixture.prefix.join("unexpected").exists());
    assert_eq!(
        entry.directories.len(),
        super::super::capacity::MAX_DIRECTORIES
    );
    assert_eq!(
        disk::read(entry.directory().join("journal.json")).unwrap(),
        before
    );
    journal::read(&fixture.paths, &entry.directory()).unwrap();
}
