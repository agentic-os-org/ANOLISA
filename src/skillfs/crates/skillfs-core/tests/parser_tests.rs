//! Integration tests for the parser module using fixture files.

use skillfs_core::parser;

mod common;
use common::load_fixture;

#[test]
fn test_parse_valid_full_fixture() {
    let content = load_fixture("valid_full.md");
    let entry = parser::parse_skill_md(&content, "web-search");

    assert_eq!(entry.metadata.name, "web-search");
    assert_eq!(
        entry.metadata.description,
        "Search the web for current information on any topic"
    );
    assert_eq!(entry.metadata.version, "1.2.0");
    assert_eq!(entry.metadata.tags, vec!["search", "web", "information"]);
    assert!(entry.metadata.enabled);
    assert_eq!(entry.parameters.len(), 3);
    assert_eq!(entry.returns.len(), 2);
    assert!(entry.parse_status.is_ok());
}

#[test]
fn test_parse_valid_minimal_fixture() {
    let content = load_fixture("valid_minimal.md");
    let entry = parser::parse_skill_md(&content, "hello-world");

    assert_eq!(entry.metadata.name, "hello-world");
    assert_eq!(
        entry.metadata.description,
        "A minimal skill that greets the user"
    );
    assert_eq!(entry.metadata.version, "0.0.0"); // default
    assert!(entry.metadata.tags.is_empty());
    assert!(entry.metadata.enabled);
    assert!(entry.parameters.is_empty());
    assert!(entry.returns.is_empty());
    assert!(entry.parse_status.is_ok());
}

#[test]
fn test_parse_valid_no_params_fixture() {
    let content = load_fixture("valid_no_params.md");
    let entry = parser::parse_skill_md(&content, "no-params-skill");

    assert!(entry.parameters.is_empty());
    assert!(entry.parse_status.is_ok());
}

#[test]
fn test_parse_missing_frontmatter_fixture() {
    let content = load_fixture("missing_frontmatter.md");
    let entry = parser::parse_skill_md(&content, "fallback-name");

    assert_eq!(entry.metadata.name, "fallback-name"); // from dir_name
    assert!(entry.parse_status.is_degraded());
}

#[test]
fn test_parse_empty_frontmatter_fixture() {
    let content = load_fixture("empty_frontmatter.md");
    let entry = parser::parse_skill_md(&content, "fallback-name");

    assert_eq!(entry.metadata.name, "fallback-name"); // from dir_name
    assert!(entry.parse_status.is_degraded());
}

#[test]
fn test_parse_invalid_yaml_fixture() {
    let content = load_fixture("invalid_yaml.md");
    let entry = parser::parse_skill_md(&content, "fallback-name");

    assert_eq!(entry.metadata.name, "fallback-name");
    assert!(entry.parse_status.is_error());
}

#[test]
fn whitespace_only_frontmatter_matches_empty_frontmatter() {
    let empty = parser::parse_skill_md("---\n\n---\nBody\n", "fallback-name");
    for whitespace in ["\t", " ", "  ", " \n\t ", "\r\n \t\r"] {
        let content = format!("---\n{whitespace}\n---\nBody\n");
        let entry = parser::parse_skill_md(&content, "fallback-name");
        assert!(
            entry.parse_status.is_degraded(),
            "{whitespace:?}: {:?}",
            entry.parse_status
        );
        assert_eq!(entry.parse_status, empty.parse_status);
        assert_eq!(entry.metadata.name, empty.metadata.name);
        assert_eq!(entry.metadata.description, empty.metadata.description);
        assert_eq!(entry.body, empty.body);
    }
}

#[test]
fn test_parse_missing_name_fixture() {
    let content = load_fixture("missing_name.md");
    let entry = parser::parse_skill_md(&content, "fallback-name");

    assert_eq!(entry.metadata.name, "fallback-name"); // from dir_name
    assert!(entry.parse_status.is_degraded());
}

#[test]
fn test_parse_missing_description_fixture() {
    let content = load_fixture("missing_description.md");
    let entry = parser::parse_skill_md(&content, "no-desc");

    assert_eq!(entry.metadata.name, "no-desc");
    // Description should be extracted from body
    assert!(!entry.metadata.description.is_empty());
    assert!(entry.parse_status.is_degraded());
}

#[test]
fn test_parse_long_description_fixture() {
    let content = load_fixture("long_description.md");
    let entry = parser::parse_skill_md(&content, "long-desc");

    assert!(entry.parse_status.is_ok());
}

#[test]
fn test_parse_mixed_params_fixture() {
    let content = load_fixture("mixed_params.md");
    let entry = parser::parse_skill_md(&content, "mixed-params");

    // Should parse some valid params and mark as degraded
    assert!(!entry.parameters.is_empty());
    assert!(entry.parse_status.is_degraded());
}

#[test]
fn test_parse_empty_fixture() {
    let content = load_fixture("empty.md");
    let entry = parser::parse_skill_md(&content, "empty-skill");

    assert!(entry.parse_status.is_error());
}

#[test]
fn test_parse_anthropic_compatible_fixture() {
    let content = load_fixture("anthropic_compatible.md");
    let entry = parser::parse_skill_md(&content, "anthropic-skill");

    // Should parse successfully (Anthropic format is a subset of our format)
    assert!(!entry.metadata.name.is_empty());
    assert!(!entry.metadata.description.is_empty());
}

#[test]
fn test_parse_skill_file_directly() {
    let fixture_path = common::fixture_path("valid_full.md");
    let entry = parser::parse_skill_file(&fixture_path).expect("should parse file");

    assert_eq!(entry.metadata.name, "web-search");
    assert!(entry.source_path.ends_with("valid_full.md"));
}

#[test]
fn test_parse_file_exceeding_default_limit_is_rejected() {
    // large_file.md (1,060,479 bytes) was checked in specifically to exceed
    // the 1 MiB default, but nothing referenced it: the FileTooLarge guard
    // had no coverage. The rejection must happen before reading the body.
    let fixture_path = common::fixture_path("large_file.md");
    let err =
        parser::parse_skill_file(&fixture_path).expect_err("oversized skill file must be rejected");
    match err {
        parser::ParseError::FileTooLarge { size, max } => {
            assert_eq!(max, 1_048_576, "default limit must be 1 MiB");
            assert!(size > max, "fixture must exceed the limit, got {size}");
        }
        other => panic!("expected FileTooLarge, got {other:?}"),
    }
}

#[test]
fn test_parse_large_file_under_raised_limit_succeeds() {
    // The same fixture parses once the configured limit admits it: the
    // guard is size-driven, not content-driven (store.rs passes
    // config.max_skill_size through parse_skill_file_with_limit).
    let fixture_path = common::fixture_path("large_file.md");
    let entry = parser::parse_skill_file_with_limit(&fixture_path, 2 * 1_048_576)
        .expect("fixture must parse under a raised limit");

    assert_eq!(entry.metadata.name, "large-skill");
    assert_eq!(entry.metadata.description, "A very large skill file");
    assert!(
        entry.parse_status.is_ok(),
        "valid frontmatter must not degrade: {:?}",
        entry.parse_status
    );
}

#[test]
fn test_size_guard_fires_before_reading_the_body() {
    // A file that is over the limit AND not valid UTF-8 can only answer
    // FileTooLarge when the guard runs on metadata before any read: a
    // read-first implementation would surface a decode/IO error instead,
    // and a huge skill file would be fully allocated before rejection.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("SKILL.md");
    let mut body = vec![0xff, 0xfe];
    body.resize(64, b'x');
    std::fs::write(&path, &body).expect("write oversized invalid-utf8 file");

    let err = parser::parse_skill_file_with_limit(&path, 32)
        .expect_err("oversized file must be rejected before reading");
    assert!(
        matches!(err, parser::ParseError::FileTooLarge { size: 64, max: 32 }),
        "guard must fire on metadata first, got {err:?}"
    );
}

#[test]
fn test_limit_boundary_is_exclusive() {
    // The guard is `size > max`: a file exactly at the limit is legal and
    // must parse. Pinning the boundary keeps a `>=` regression from
    // silently rejecting legitimate at-limit skill files.
    let fixture_path = common::fixture_path("large_file.md");
    let exact = std::fs::metadata(&fixture_path)
        .expect("fixture metadata")
        .len() as usize;
    let entry = parser::parse_skill_file_with_limit(&fixture_path, exact)
        .expect("a file exactly at the limit must parse");
    assert_eq!(entry.metadata.name, "large-skill");
}
