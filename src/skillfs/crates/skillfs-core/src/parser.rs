use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use thiserror::Error;

use crate::{ParamType, Parameter, ParseStatus, ReturnField, SkillEntry, SkillMetadata};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("file too large: {size} bytes (max {max})")]
    FileTooLarge { size: usize, max: usize },
}

/// Parse a SKILL.md file from its content string.
///
/// `dir_name` is used as fallback for the skill name when frontmatter is missing.
///
/// This function **never** returns `Err` — it always produces a `SkillEntry`
/// with an appropriate `ParseStatus`.
pub fn parse_skill_md(content: &str, dir_name: &str) -> SkillEntry {
    // A UTF-8 BOM (emitted by some editors) must not defeat fence
    // detection: strip it before any content inspection.
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);

    // Handle empty content
    if content.trim().is_empty() {
        let metadata = SkillMetadata {
            name: dir_name.to_string(),
            description: String::new(),
            ..Default::default()
        };
        return SkillEntry {
            metadata,
            parameters: Vec::new(),
            returns: Vec::new(),
            body: String::new(),
            parse_status: ParseStatus::Error("empty content".to_string()),
            source_path: std::path::PathBuf::new(),
            last_modified: std::time::SystemTime::UNIX_EPOCH,
        };
    }

    let mut issues: Vec<String> = Vec::new();

    // Phase 1: Frontmatter extraction
    let (yaml_str, body) = extract_frontmatter(content);

    // Phase 2: Frontmatter parsing
    let metadata = parse_frontmatter(&yaml_str, &body, dir_name, &mut issues);

    // Phase 3: Section splitting
    let sections = split_sections(&body);

    // Phase 4: Structured extraction
    let parameters = parse_parameters(sections.get("Parameters"), &mut issues);
    let returns = parse_returns(sections.get("Returns"), &mut issues);

    // Determine parse status
    let parse_status = if issues.is_empty() {
        ParseStatus::Ok
    } else if issues.iter().any(|i| i.starts_with("invalid YAML")) {
        ParseStatus::Error(issues.join("; "))
    } else {
        ParseStatus::Degraded(issues.join("; "))
    };

    SkillEntry {
        metadata,
        parameters,
        returns,
        body,
        parse_status,
        source_path: std::path::PathBuf::new(),
        last_modified: std::time::SystemTime::UNIX_EPOCH,
    }
}

/// Parse a SKILL.md file from a filesystem path.
pub fn parse_skill_file(path: &Path) -> Result<SkillEntry, ParseError> {
    parse_skill_file_with_limit(path, 1_048_576)
}

/// Parse from file with explicit size limit.
pub fn parse_skill_file_with_limit(path: &Path, max_size: usize) -> Result<SkillEntry, ParseError> {
    let file_meta = std::fs::metadata(path)?;
    let size = file_meta.len() as usize;
    if size > max_size {
        return Err(ParseError::FileTooLarge {
            size,
            max: max_size,
        });
    }

    let content = std::fs::read_to_string(path)?;
    let dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut entry = parse_skill_md(&content, dir_name);
    entry.source_path = path.to_path_buf();
    entry.last_modified = file_meta
        .modified()
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    Ok(entry)
}

// ---------------------------------------------------------------------------
// Phase 1: Frontmatter Extraction
// ---------------------------------------------------------------------------

fn extract_frontmatter(content: &str) -> (String, String) {
    if !content.starts_with("---") {
        return (String::new(), content.to_string());
    }

    // Find the closing "---" after the opening one
    let after_open = &content[3..];
    // Skip the newline after opening ---
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);

    if let Some(close_pos) = after_open.find("\n---") {
        let yaml = after_open[..close_pos].to_string();
        let rest_start = close_pos + 4; // skip "\n---"
        let body = if rest_start < after_open.len() {
            let rest = &after_open[rest_start..];
            rest.strip_prefix('\n').unwrap_or(rest).to_string()
        } else {
            String::new()
        };
        (yaml, body)
    } else if after_open.ends_with("---") && !after_open.contains('\n') {
        // Edge case: "---\n---" with no content
        (String::new(), String::new())
    } else {
        // No closing ---, treat entire content as body
        (String::new(), content.to_string())
    }
}

// ---------------------------------------------------------------------------
// Phase 2: Frontmatter Parsing
// ---------------------------------------------------------------------------

fn parse_frontmatter(
    yaml_str: &str,
    body: &str,
    dir_name: &str,
    issues: &mut Vec<String>,
) -> SkillMetadata {
    if yaml_str.is_empty() {
        if !dir_name.is_empty() {
            issues.push("missing frontmatter".to_string());
        }
        let desc = extract_first_paragraph(body);
        return SkillMetadata {
            name: dir_name.to_string(),
            description: desc,
            ..Default::default()
        };
    }

    match serde_yaml::from_str::<SkillMetadata>(yaml_str) {
        Ok(mut meta) => {
            // Name fallback - if name is missing in YAML, it will be empty string
            if meta.name.is_empty() {
                meta.name = dir_name.to_string();
                issues.push("missing name field in frontmatter".to_string());
            }
            // Name validation
            validate_name(&meta.name, issues);
            // Description fallback
            if meta.description.is_empty() {
                meta.description = extract_first_paragraph(body);
                issues.push("missing description".to_string());
            }
            meta
        }
        Err(e) => {
            issues.push(format!("invalid YAML: {e}"));
            let desc = extract_first_paragraph(body);
            SkillMetadata {
                name: dir_name.to_string(),
                description: desc,
                ..Default::default()
            }
        }
    }
}

pub(crate) fn validate_name(name: &str, issues: &mut Vec<String>) {
    if name.len() > 64 {
        issues.push("name too long (max 64 chars)".to_string());
    }
    if !is_kebab_case(name) {
        issues.push("name not kebab-case".to_string());
    }
}

/// Non-empty kebab-case identifier (`[a-z0-9-]`, no leading/trailing
/// hyphen). Length is enforced separately by [`validate_name`]; this is
/// the single grammar definition, shared with the store's
/// directory-name adoption check.
pub(crate) fn is_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn extract_first_paragraph(body: &str) -> String {
    let trimmed = body.trim_start();
    // Skip leading heading
    let content = if trimmed.starts_with('#') {
        trimmed
            .find('\n')
            .map(|pos| trimmed[pos + 1..].trim_start())
            .unwrap_or("")
    } else {
        trimmed
    };
    // Take first non-empty paragraph
    content
        .split("\n\n")
        .find(|p| !p.trim().is_empty())
        .unwrap_or("")
        .lines()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Phase 3: Section Splitting
// ---------------------------------------------------------------------------

fn split_sections(body: &str) -> std::collections::HashMap<String, String> {
    let mut sections = std::collections::HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_content = String::new();

    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some(name) = current_name.take() {
                sections.insert(name, current_content.trim().to_string());
            }
            current_name = Some(heading.trim().to_string());
            current_content = String::new();
        } else if current_name.is_some() {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    if let Some(name) = current_name {
        sections.insert(name, current_content.trim().to_string());
    }

    sections
}

// ---------------------------------------------------------------------------
// Phase 4: Structured Extraction
// ---------------------------------------------------------------------------

fn parse_parameters(section: Option<&String>, issues: &mut Vec<String>) -> Vec<Parameter> {
    let section = match section {
        Some(s) => s,
        None => return Vec::new(),
    };
    parse_typed_list(section, issues)
}

fn parse_returns(section: Option<&String>, issues: &mut Vec<String>) -> Vec<ReturnField> {
    let section = match section {
        Some(s) => s,
        None => return Vec::new(),
    };

    let params = parse_typed_list(section, issues);
    params
        .into_iter()
        .map(|p| ReturnField {
            name: p.name,
            field_type: p.param_type,
            description: p.description,
        })
        .collect()
}

/// `- \`name\` (type, required|optional): description`. Compiled once:
/// SKILL.md re-parses run on every FUSE write event (sync worker) and on
/// every store refresh, so per-call compilation is wasted work.
///
/// Names are kebab-case per `validate_name` (`[a-z0-9-]`), so the name group
/// admits hyphens (`max-results`); `\w+` alone would flag those lines as
/// malformed and drop the parameter.
static TYPED_LIST_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Static literal pattern; compilation cannot fail at runtime.
    Regex::new(r"^-\s+`([\w-]+)`\s+\((\w+)(?:,\s*(required|optional))?\):\s*(.*)$")
        .expect("static literal pattern")
});

/// Parse lines like: - `name` (type, required|optional): description
fn parse_typed_list(section: &str, issues: &mut Vec<String>) -> Vec<Parameter> {
    let re = &*TYPED_LIST_RE;

    let mut result = Vec::new();
    for line in section.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("- ") {
            continue;
        }
        if let Some(caps) = re.captures(trimmed) {
            let name = caps[1].to_string();
            let type_str = &caps[2];
            let required_str = caps.get(3).map(|m| m.as_str());
            let desc = caps[4].trim().to_string();

            match ParamType::from_str_opt(type_str) {
                Some(param_type) => {
                    let required = required_str == Some("required");
                    result.push(Parameter {
                        name,
                        param_type,
                        required,
                        description: desc,
                    });
                }
                None => {
                    issues.push(format!("unknown parameter type: {type_str}"));
                }
            }
        } else if trimmed.starts_with("- `") || trimmed.starts_with("- ") {
            // Any line starting with "- " that didn't match the pattern is malformed
            issues.push(format!("malformed parameter line: {trimmed}"));
        }
        // Other lines are silently skipped (free-form content)
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Basic Parsing Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_full_valid() {
        let content = r#"---
name: web-search
description: Search the web for current information on any topic
version: 1.2.0
tags:
  - search
  - web
  - information
enabled: true
---

# Web Search

Search the web for current information using multiple search engines.

## Parameters

- `query` (string, required): The search query
- `count` (integer, optional): Maximum number of results to return

## Returns

- `results` (array, required): List of search result objects
"#;

        let entry = parse_skill_md(content, "web-search");

        assert_eq!(entry.metadata.name, "web-search");
        assert_eq!(
            entry.metadata.description,
            "Search the web for current information on any topic"
        );
        assert_eq!(entry.metadata.version, "1.2.0");
        assert_eq!(entry.metadata.tags, vec!["search", "web", "information"]);
        assert!(entry.metadata.enabled);
        assert_eq!(entry.parameters.len(), 2);
        assert_eq!(entry.returns.len(), 1);
        assert!(entry.parse_status.is_ok());
    }

    #[test]
    fn test_parse_minimal_valid() {
        let content = r#"---
name: hello-world
description: A minimal skill that greets the user
---

Say hello to the world.
"#;

        let entry = parse_skill_md(content, "hello-world");

        assert_eq!(entry.metadata.name, "hello-world");
        assert_eq!(
            entry.metadata.description,
            "A minimal skill that greets the user"
        );
        assert_eq!(entry.metadata.version, "0.0.0"); // default
        assert!(entry.metadata.tags.is_empty()); // default
        assert!(entry.metadata.enabled); // default
        assert!(entry.parameters.is_empty());
        assert!(entry.returns.is_empty());
        assert!(entry.parse_status.is_ok());
    }

    // -----------------------------------------------------------------------
    // Edge Case Tests - Frontmatter
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_no_frontmatter() {
        let content = r#"# Web Search

Search the web for current information.
"#;

        let entry = parse_skill_md(content, "web-search");

        assert_eq!(entry.metadata.name, "web-search"); // from dir_name
        assert!(entry.parse_status.is_degraded());
    }

    #[test]
    fn test_parse_bom_prefixed_frontmatter() {
        let content =
            "\u{feff}---\nname: web-search\ndescription: Search the web\n---\n\n# Web Search\n";

        let entry = parse_skill_md(content, "fallback-dir");

        assert_eq!(
            entry.metadata.name, "web-search",
            "frontmatter after a BOM must be honored, not discarded"
        );
        assert_eq!(entry.metadata.description, "Search the web");
        assert!(
            entry.parse_status.is_ok(),
            "BOM-prefixed skill must parse cleanly, got {:?}",
            entry.parse_status
        );
    }

    #[test]
    fn test_parse_empty_frontmatter() {
        let content = r#"---
---

# Web Search

Search the web.
"#;

        let entry = parse_skill_md(content, "web-search");

        assert_eq!(entry.metadata.name, "web-search"); // from dir_name
        assert!(entry.parse_status.is_degraded());
    }

    #[test]
    fn test_parse_invalid_yaml() {
        let content = r#"---
name: test
description: : invalid yaml here
---

Body.
"#;

        let entry = parse_skill_md(content, "test");

        assert_eq!(entry.metadata.name, "test"); // from dir_name
        assert!(entry.parse_status.is_error());
    }

    #[test]
    fn test_parse_missing_name() {
        let content = r#"---
description: A skill without a name
---

Body.
"#;

        let entry = parse_skill_md(content, "fallback-name");

        assert_eq!(entry.metadata.name, "fallback-name"); // from dir_name
        assert!(entry.parse_status.is_degraded());
    }

    #[test]
    fn test_parse_missing_description() {
        let content = r#"---
name: test-skill
---

This is the first paragraph that should be used as description.

More content here.
"#;

        let entry = parse_skill_md(content, "test-skill");

        assert_eq!(entry.metadata.name, "test-skill");
        // Description should be extracted from first paragraph
        assert!(!entry.metadata.description.is_empty());
        assert!(entry.parse_status.is_degraded());
    }

    #[test]
    fn test_parse_empty_content() {
        let content = "";

        let entry = parse_skill_md(content, "empty-skill");

        assert_eq!(entry.metadata.name, "empty-skill");
        assert!(entry.parse_status.is_error());
    }

    // -----------------------------------------------------------------------
    // Parameter Parsing Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_parameters_valid() {
        let content = r#"---
name: test
description: Test skill
---

## Parameters

- `query` (string, required): The search query
- `count` (integer, optional): Number of results
- `enabled` (boolean, required): Whether to enable
"#;

        let entry = parse_skill_md(content, "test");

        assert_eq!(entry.parameters.len(), 3);
        assert_eq!(entry.parameters[0].name, "query");
        assert_eq!(entry.parameters[0].param_type, ParamType::String);
        assert!(entry.parameters[0].required);
        assert_eq!(entry.parameters[1].name, "count");
        assert_eq!(entry.parameters[1].param_type, ParamType::Integer);
        assert!(!entry.parameters[1].required);
    }

    #[test]
    fn test_parse_parameters_mixed() {
        let content = r#"---
name: test
description: Test skill
---

## Parameters

- `valid` (string, required): A valid parameter
- invalid line without proper format
- `another` (integer, optional): Another valid one
"#;

        let entry = parse_skill_md(content, "test");

        assert_eq!(entry.parameters.len(), 2); // only valid ones
        assert!(entry.parse_status.is_degraded());
    }

    #[test]
    fn test_parse_parameters_missing_section() {
        let content = r#"---
name: test
description: Test skill without parameters
---

Just body content.
"#;

        let entry = parse_skill_md(content, "test");

        assert!(entry.parameters.is_empty());
        assert!(entry.parse_status.is_ok());
    }

    #[test]
    fn test_parse_parameters_type_unknown() {
        let content = r#"---
name: test
description: Test skill
---

## Parameters

- `valid` (string, required): Valid param
- `unknown` (unknowntype, optional): Unknown type
"#;

        let entry = parse_skill_md(content, "test");

        // Unknown type param should be skipped
        assert_eq!(entry.parameters.len(), 1);
        assert_eq!(entry.parameters[0].name, "valid");
        assert!(entry.parse_status.is_degraded());
    }

    #[test]
    fn test_parse_parameters_kebab_case_names() {
        let content = r#"---
name: test
description: Test skill
---

## Parameters

- `max-results` (integer, optional): Cap on results
- `dry-run` (boolean, required): Run without side effects
"#;

        let entry = parse_skill_md(content, "test");

        // kebab-case is the naming convention validate_name enforces; such
        // parameters must parse instead of being flagged malformed.
        assert_eq!(entry.parameters.len(), 2);
        assert_eq!(entry.parameters[0].name, "max-results");
        assert_eq!(entry.parameters[1].name, "dry-run");
        assert!(entry.parse_status.is_ok());
    }

    #[test]
    fn test_parse_parameters_missing_required_optional() {
        let content = r#"---
name: test
description: Test skill
---

## Parameters

- `param1` (string): No required/optional marker
"#;

        let entry = parse_skill_md(content, "test");

        assert_eq!(entry.parameters.len(), 1);
        assert!(!entry.parameters[0].required); // default to optional
    }

    // -----------------------------------------------------------------------
    // Returns Parsing Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_returns_valid() {
        let content = r#"---
name: test
description: Test skill
---

## Returns

- `result` (string, required): The result
- `count` (integer, required): The count
"#;

        let entry = parse_skill_md(content, "test");

        assert_eq!(entry.returns.len(), 2);
        assert_eq!(entry.returns[0].name, "result");
        assert_eq!(entry.returns[0].field_type, ParamType::String);
    }

    #[test]
    fn test_parse_returns_missing() {
        let content = r#"---
name: test
description: Test skill
---

No returns section here.
"#;

        let entry = parse_skill_md(content, "test");

        assert!(entry.returns.is_empty());
        assert!(entry.parse_status.is_ok());
    }

    // -----------------------------------------------------------------------
    // Name Validation Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_name_validation_kebab() {
        let content = r#"---
name: web-search
description: Test
---

Body.
"#;

        let entry = parse_skill_md(content, "web-search");

        assert_eq!(entry.metadata.name, "web-search");
        assert!(entry.parse_status.is_ok());
    }

    #[test]
    fn test_name_validation_too_long() {
        let long_name = "a".repeat(65);
        let content = format!(
            r#"---
name: {}
description: Test
---

Body.
"#,
            long_name
        );

        let entry = parse_skill_md(&content, "fallback");

        assert!(entry.parse_status.is_degraded());
    }

    #[test]
    fn test_name_validation_non_kebab() {
        let content = r#"---
name: Web_Search
description: Test
---

Body.
"#;

        let entry = parse_skill_md(content, "web-search");

        assert!(entry.parse_status.is_degraded());
    }

    // -----------------------------------------------------------------------
    // Edge Case Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_frontmatter_extraction_no_trailing_newline() {
        let content = "---\nname: test\ndescription: Test\n---"; // no newline after ---

        let entry = parse_skill_md(content, "test");

        assert_eq!(entry.metadata.name, "test");
    }
}
