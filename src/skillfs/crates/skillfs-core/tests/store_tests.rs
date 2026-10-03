//! Integration tests for the store module using fixture directories.

mod common;
use std::path::{Path, PathBuf};

use common::load_fixture;
use skillfs_core::{ParseConfig, store::SkillStore};

fn add_skill(source_dir: &Path, name: &str, content: &str) -> PathBuf {
    let skill_dir = source_dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    let skill_md = skill_dir.join("SKILL.md");
    std::fs::write(&skill_md, content).unwrap();
    skill_md
}

fn add_skill_from_fixture(source_dir: &Path, name: &str, fixture_name: &str) -> PathBuf {
    let content = load_fixture(fixture_name);
    add_skill(source_dir, name, &content)
}

fn load_store(source_dir: &Path) -> (SkillStore, Vec<skillfs_core::store::LoadError>) {
    let mut store = SkillStore::new();
    let errors = store.load_from_directory(source_dir, &ParseConfig::default());
    (store, errors)
}

#[test]
fn test_load_from_directory_with_valid_skills() {
    let source_dir = tempfile::tempdir().unwrap();

    add_skill_from_fixture(source_dir.path(), "web-search-dir", "valid_full.md");
    add_skill_from_fixture(source_dir.path(), "hello-world-dir", "valid_minimal.md");
    add_skill_from_fixture(source_dir.path(), "code-review-dir", "valid_no_params.md");

    let (store, _errors) = load_store(source_dir.path());

    assert_eq!(store.len(), 3);
    // Names come from directory basename, not SKILL.md frontmatter
    assert!(store.get("web-search-dir").is_some());
    assert!(store.get("hello-world-dir").is_some());
    assert!(store.get("code-review-dir").is_some());
}

#[test]
fn test_load_from_directory_with_mixed_quality() {
    let source_dir = tempfile::tempdir().unwrap();

    // Add skills with various parse statuses
    // Note: skill names come from file content, not directory names
    add_skill_from_fixture(source_dir.path(), "valid-dir", "valid_full.md");
    add_skill_from_fixture(source_dir.path(), "degraded-dir", "missing_frontmatter.md");
    add_skill_from_fixture(source_dir.path(), "errored-dir", "invalid_yaml.md");

    let (store, _errors) = load_store(source_dir.path());

    // All should be loaded (even degraded/errored)
    assert_eq!(store.len(), 3);

    // Check parse statuses by iterating (names come from file content)
    let skills: Vec<_> = store.list();
    assert_eq!(skills.len(), 3);
}

#[test]
fn test_load_from_directory_ignores_hidden() {
    let source_dir = tempfile::tempdir().unwrap();

    add_skill_from_fixture(source_dir.path(), "visible-dir", "valid_minimal.md");

    // Create a hidden directory with a skill
    let hidden_dir = source_dir.path().join(".hidden");
    std::fs::create_dir(&hidden_dir).unwrap();
    std::fs::write(hidden_dir.join("SKILL.md"), "---\nname: hidden\n---\n").unwrap();

    let (store, _errors) = load_store(source_dir.path());

    assert_eq!(store.len(), 1);
    // Name comes from directory basename, not SKILL.md frontmatter
    assert!(store.get("visible-dir").is_some());
    assert!(store.get("hidden").is_none());
}

#[test]
fn test_load_from_directory_ignores_files() {
    let source_dir = tempfile::tempdir().unwrap();

    add_skill_from_fixture(source_dir.path(), "valid", "valid_minimal.md");

    // Create a file (not directory) in source
    std::fs::write(source_dir.path().join("not-a-dir.txt"), "not a skill").unwrap();

    let (store, errors) = load_store(source_dir.path());

    assert!(errors.is_empty());
    assert_eq!(store.len(), 1);
}

#[test]
fn test_load_from_directory_skips_no_skill_md() {
    let source_dir = tempfile::tempdir().unwrap();

    add_skill_from_fixture(source_dir.path(), "valid", "valid_minimal.md");

    // Create a directory without SKILL.md
    let empty_dir = source_dir.path().join("empty-dir");
    std::fs::create_dir(&empty_dir).unwrap();
    std::fs::write(empty_dir.join("README.md"), "not a skill file").unwrap();

    let (store, errors) = load_store(source_dir.path());

    assert!(errors.is_empty());
    assert_eq!(store.len(), 1);
}

#[test]
fn test_load_from_directory_empty() {
    let source_dir = tempfile::tempdir().unwrap();

    let (store, errors) = load_store(source_dir.path());

    assert!(errors.is_empty());
    assert!(store.is_empty());
}

#[test]
fn test_reload_updates_existing() {
    let source_dir = tempfile::tempdir().unwrap();

    // First load
    add_skill(
        source_dir.path(),
        "test-skill",
        "---\nname: test-skill\ndescription: Original\n---\n",
    );
    let (store, _errors) = load_store(source_dir.path());
    assert_eq!(
        store.get("test-skill").unwrap().metadata.description,
        "Original"
    );

    // Update the skill file
    let skill_path = source_dir.path().join("test-skill").join("SKILL.md");
    std::fs::write(
        &skill_path,
        "---\nname: test-skill\ndescription: Updated\n---\n",
    )
    .unwrap();

    // Reload
    let (store, _errors) = load_store(source_dir.path());

    assert_eq!(
        store.get("test-skill").unwrap().metadata.description,
        "Updated"
    );
}

// -----------------------------------------------------------------------
// Canonical identity: directory basename overrides frontmatter name
// -----------------------------------------------------------------------

#[test]
fn flat_layout_uses_directory_basename_not_frontmatter_name() {
    let source_dir = tempfile::tempdir().unwrap();

    add_skill(
        source_dir.path(),
        "tianqi-weather",
        "---\nname: 天气\ndescription: weather skill\n---\n",
    );

    let (store, errors) = load_store(source_dir.path());

    assert!(errors.is_empty());
    assert_eq!(store.len(), 1);
    let names = store.list();
    assert!(
        names.contains(&"tianqi-weather"),
        "store key must be directory basename, got {names:?}"
    );
    assert!(
        !names.contains(&"天气"),
        "frontmatter name must NOT appear as store key, got {names:?}"
    );
    let entry = store.get("tianqi-weather").unwrap();
    assert_eq!(entry.metadata.name, "tianqi-weather");
    assert!(store.get("天气").is_none());
}

#[test]
fn categorized_layout_uses_directory_basename_not_frontmatter_name() {
    let source_dir = tempfile::tempdir().unwrap();

    // Create a category directory with a skill inside
    let cat_dir = source_dir.path().join("weather");
    let skill_dir = cat_dir.join("tianqi-weather");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: 天气\ndescription: weather skill\n---\n",
    )
    .unwrap();

    let (store, errors) = load_store(source_dir.path());

    assert!(errors.is_empty());
    assert_eq!(store.len(), 1);
    let names = store.list();
    assert!(
        names.contains(&"tianqi-weather"),
        "store key must be skill directory basename, got {names:?}"
    );
    assert!(
        !names.contains(&"天气"),
        "frontmatter name must NOT appear as store key, got {names:?}"
    );
    let entry = store.get("tianqi-weather").unwrap();
    assert_eq!(entry.metadata.name, "tianqi-weather");
    assert!(store.get("天气").is_none());
}

// -----------------------------------------------------------------------
// Adopted directory names must obey the skill-name grammar
// -----------------------------------------------------------------------

/// Mirrors the grammar the parser enforces for skill names: non-empty
/// kebab-case (`[a-z0-9-]`), no leading/trailing hyphen, max 64 chars.
fn is_kebab_case_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

#[test]
fn invalid_directory_name_degrades_the_skill() {
    let source_dir = tempfile::tempdir().unwrap();

    // Underscored directory name with fully valid frontmatter: the store
    // still adopts the directory name as the skill identity, so the entry
    // must be degraded rather than presented as cleanly parsed.
    add_skill(
        source_dir.path(),
        "foo_bar",
        "---\nname: foo-bar\ndescription: valid frontmatter\n---\n",
    );

    let (store, errors) = load_store(source_dir.path());

    assert!(errors.is_empty());
    assert_eq!(store.len(), 1);
    let entry = store.get("foo_bar").expect("skill must not be skipped");
    assert!(
        entry.parse_status.is_degraded(),
        "non-kebab directory name must degrade the entry, got {:?}",
        entry.parse_status
    );
}

#[test]
fn stored_skill_names_are_kebab_or_degraded() {
    let source_dir = tempfile::tempdir().unwrap();

    add_skill(
        source_dir.path(),
        "good-skill",
        "---\nname: good-skill\ndescription: fine\n---\n",
    );
    add_skill(
        source_dir.path(),
        "foo_bar",
        "---\nname: foo-bar\ndescription: valid frontmatter\n---\n",
    );

    // Same invariant for the categorized layout loader.
    let cat_dir = source_dir.path().join("weather");
    let nested_good = cat_dir.join("nested-good");
    std::fs::create_dir_all(&nested_good).unwrap();
    std::fs::write(
        nested_good.join("SKILL.md"),
        "---\nname: nested-good\ndescription: fine\n---\n",
    )
    .unwrap();
    let nested_bad = cat_dir.join("Nested_Bad");
    std::fs::create_dir_all(&nested_bad).unwrap();
    std::fs::write(
        nested_bad.join("SKILL.md"),
        "---\nname: nested-bad\ndescription: fine\n---\n",
    )
    .unwrap();

    let (store, _errors) = load_store(source_dir.path());

    assert_eq!(store.len(), 4);
    for (name, entry) in store.iter() {
        assert!(
            entry.parse_status.is_degraded() || is_kebab_case_skill_name(name),
            "skill `{name}` is neither kebab-case nor degraded: {:?}",
            entry.parse_status
        );
    }
}

// -----------------------------------------------------------------------
// Shared directory-name adoption entry point
// -----------------------------------------------------------------------

/// The loaders, the FUSE sync worker, and the rename path all adopt a
/// directory name through `store::adopt_directory_name`; these tests pin
/// the contract of that shared entry point directly.
fn make_entry(parse_status: skillfs_core::ParseStatus) -> skillfs_core::SkillEntry {
    skillfs_core::SkillEntry {
        metadata: skillfs_core::SkillMetadata {
            name: "frontmatter-name".to_string(),
            ..skillfs_core::SkillMetadata::default()
        },
        parameters: vec![],
        returns: vec![],
        body: String::new(),
        parse_status,
        source_path: std::path::PathBuf::new(),
        last_modified: std::time::SystemTime::UNIX_EPOCH,
    }
}

#[test]
fn adopt_directory_name_valid_name_stays_clean() {
    let mut entry = make_entry(skillfs_core::ParseStatus::Ok);
    skillfs_core::store::adopt_directory_name(&mut entry, "good-skill");
    assert_eq!(entry.metadata.name, "good-skill");
    assert!(entry.parse_status.is_ok());
}

#[test]
fn adopt_directory_name_invalid_dir_degrades_clean_entry() {
    let mut entry = make_entry(skillfs_core::ParseStatus::Ok);
    skillfs_core::store::adopt_directory_name(&mut entry, "foo_bar");
    assert_eq!(entry.metadata.name, "foo_bar");
    assert!(entry.parse_status.is_degraded());
    let msg = entry.parse_status.message();
    assert!(
        msg.contains("foo_bar"),
        "issue must name the directory: {msg}"
    );
    assert!(msg.contains("kebab"), "issue must name the grammar: {msg}");
}

#[test]
fn adopt_directory_name_merges_with_existing_degradation() {
    let mut entry = make_entry(skillfs_core::ParseStatus::Degraded(
        "pre-existing issue".to_string(),
    ));
    skillfs_core::store::adopt_directory_name(&mut entry, "foo_bar");
    let msg = entry.parse_status.message();
    assert!(
        msg.contains("pre-existing issue") && msg.contains("foo_bar"),
        "degradations must merge, got: {msg}"
    );
}

#[test]
fn adopt_directory_name_preserves_error_status() {
    let mut entry = make_entry(skillfs_core::ParseStatus::Error("boom".to_string()));
    skillfs_core::store::adopt_directory_name(&mut entry, "foo_bar");
    assert_eq!(entry.metadata.name, "foo_bar");
    assert!(entry.parse_status.is_error());
}
