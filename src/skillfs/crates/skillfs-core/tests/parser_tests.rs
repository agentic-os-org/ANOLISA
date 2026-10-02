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
