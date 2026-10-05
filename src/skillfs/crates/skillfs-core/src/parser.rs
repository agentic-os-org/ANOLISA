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
    let sections = split_sections(&body, &mut issues);

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
    // The opening fence must be the entire first line: `---` plus at most
    // trailing whitespace (a `\r` from CRLF files, or spaces an editor
    // left behind). A longer dash run (`----`, `-----`) is a Markdown
    // thematic break and `---text` is ordinary content — neither opens
    // frontmatter. This mirrors the sec-core scanner's
    // `line.trim() == "---"` rule so both components agree on what a
    // fence is; the previous prefix match treated a file opening with a
    // thematic break as frontmatter and silently dropped everything
    // between it and the next `---`-prefixed line out of the body.
    let (first_line, rest) = match content.split_once('\n') {
        Some((first, rest)) => (first, rest),
        None => (content, ""),
    };
    if first_line.trim_end() != "---" {
        return (String::new(), content.to_string());
    }

    // Find the first line of `rest` that is exactly `---` (trailing
    // whitespace tolerated, leading whitespace not — an indented `---` is
    // content). A line that merely *starts* with `---` (`----`, `---text`)
    // must NOT close the frontmatter: the previous prefix match landed the
    // body slice mid-line and leaked the remainder (a stray `-`, a space,
    // or the text) into the body.
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if bare.trim_end() == "---" {
            // yaml spans from the start of `rest` to this line, minus the
            // newline that terminates the last yaml line (byte-compatible
            // with the previous `after_open[len..close_pos]` region).
            let yaml = rest[..offset]
                .strip_suffix('\n')
                .map(|s| s.strip_suffix('\r').unwrap_or(s))
                .unwrap_or(&rest[..offset])
                .to_string();
            // The body starts after the closer line's own newline; when
            // the closer is the last line there is no body.
            return (yaml, rest[offset + line.len()..].to_string());
        }
        offset += line.len();
    }

    // No closing fence: treat the entire content as body, a bare opener
    // included (`"---"`, `"--- \n"`, `"---\r\n"` — the pre-rewrite code's
    // `after_open.ends_with("---")` arm never matched an empty remainder,
    // so these inputs always fell through to whole-content). The sec-core
    // scanner reports this shape as an unclosed-fence finding; skillfs
    // degrades to "missing frontmatter".
    (String::new(), content.to_string())
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
    // Take the first non-empty paragraph. A paragraph ends at a blank
    // line; `lines()` strips the terminator of either convention, so a
    // CRLF file yields the same paragraph as an LF one.
    let mut paragraph = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(line);
    }
    paragraph
}

// ---------------------------------------------------------------------------
// Phase 3: Section Splitting
// ---------------------------------------------------------------------------

fn split_sections(
    body: &str,
    issues: &mut Vec<String>,
) -> std::collections::HashMap<String, String> {
    let mut sections = std::collections::HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_content = String::new();

    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some(name) = current_name.take() {
                record_section(&mut sections, name, current_content, issues);
            }
            current_name = Some(heading.trim().to_string());
            current_content = String::new();
        } else if current_name.is_some() {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    if let Some(name) = current_name {
        record_section(&mut sections, name, current_content, issues);
    }

    sections
}

/// Section names that feed structured extraction. Only these form the
/// skill's contract; only they are checked for duplication.
const CONTRACT_SECTIONS: [&str; 2] = ["Parameters", "Returns"];

/// Insert one section, appending in encounter order when the heading
/// repeats. Only a repeated contract heading (`Parameters` / `Returns`)
/// is flagged as a parse issue (degrading the entry): structured
/// extraction consumes just those names, while a same-name H2 under a
/// different H1 parent is a layout real skills already use, not a
/// contract violation. The earlier content is always kept, so nothing
/// the author wrote is silently dropped.
fn record_section(
    sections: &mut std::collections::HashMap<String, String>,
    name: String,
    content: String,
    issues: &mut Vec<String>,
) {
    let content = content.trim().to_string();
    match sections.get_mut(&name) {
        Some(existing) => {
            if CONTRACT_SECTIONS.contains(&name.as_str()) {
                issues.push(format!("duplicate section heading: {name}"));
            }
            existing.push('\n');
            existing.push_str(&content);
        }
        None => {
            sections.insert(name, content);
        }
    }
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
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
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
                    if !seen.insert(name.clone()) {
                        issues.push(format!("duplicate parameter name: {name}"));
                    }
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
    fn test_parse_empty_frontmatter_does_not_leak_fences() {
        let content = "---\n---\n\n# Web Search\n\nSearch the web.\n";

        let entry = parse_skill_md(content, "web-search");

        assert!(
            !entry.body.contains("---"),
            "frontmatter fences must not leak into the body: {:?}",
            entry.body
        );
        assert_eq!(entry.metadata.description, "Search the web.");
        assert!(entry.parse_status.is_degraded()); // still missing frontmatter
    }

    #[test]
    fn test_parse_non_ascii_after_frontmatter_opener() {
        // `---é` is not a fence line under the exact-line rule (mirroring
        // the sec-core scanner's `line.trim() == "---"`), so the whole
        // file is ordinary content: the body keeps every line instead of
        // being truncated after the `---`-prefixed one. The rewrite uses
        // no fixed-index byte slicing (split_once / split_inclusive /
        // strip_suffix are char-boundary safe), so multi-byte characters
        // right after a dash run cannot panic.
        let content = "---é\n---\nbody";

        let entry = parse_skill_md(content, "demo");

        assert_eq!(entry.metadata.name, "demo");
        assert_eq!(entry.body, "---é\n---\nbody");
        assert!(entry.parse_status.is_degraded()); // no usable frontmatter
    }

    #[test]
    fn test_parse_crlf_frontmatter_metadata_extracted() {
        // Regression: strip_prefix('\n') after the opening fence returned
        // None for CRLF files, emptying the whole YAML so a valid skill
        // degraded to directory-name metadata (worked at merge-base).
        let content = "---\r\nname: web-search\r\ndescription: Search the web\r\n---\r\nBody\r\n";

        let entry = parse_skill_md(content, "dir-name");

        assert_eq!(entry.metadata.name, "web-search");
        assert_eq!(entry.metadata.description, "Search the web");
        assert!(!entry.body.contains("---"));
        assert!(entry.parse_status.is_ok());
    }

    #[test]
    fn test_parse_crlf_empty_frontmatter_does_not_leak_fences() {
        let content = "---\r\n---\r\nBody";

        let entry = parse_skill_md(content, "web-search");

        assert!(
            !entry.body.contains("---"),
            "frontmatter fences must not leak into the body: {:?}",
            entry.body
        );
        assert_eq!(entry.body, "Body");
        assert_eq!(entry.metadata.name, "web-search"); // from dir_name
        assert!(entry.parse_status.is_degraded()); // still missing frontmatter
    }

    #[test]
    fn test_parse_crlf_description_stops_at_the_blank_line() {
        // Regression: the paragraph fallback split on "\n\n", which never
        // matches CRLF text, so a Windows-authored skill without a
        // frontmatter description got its whole body back as the
        // description instead of the first paragraph.
        let content = "---\r\nname: web-search\r\n---\r\n# Web Search\r\n\r\nSearch the web for dogs.\r\n\r\nMore content here.\r\n";

        let entry = parse_skill_md(content, "web-search");

        assert_eq!(
            entry.metadata.description, "Search the web for dogs.",
            "CRLF paragraph must end at the blank line"
        );
        assert!(entry.parse_status.is_degraded()); // still missing description

        // The LF twin defines the contract: the same first paragraph.
        let lf = "---\nname: web-search\n---\n# Web Search\n\nSearch the web for dogs.\n\nMore content here.\n";
        assert_eq!(
            parse_skill_md(lf, "web-search").metadata.description,
            entry.metadata.description
        );
    }

    #[test]
    fn test_parse_crlf_without_frontmatter_uses_the_first_paragraph() {
        // The no-frontmatter fallback takes the same description path.
        let content = "# Web Search\r\n\r\nSearch the web for dogs.\r\n\r\nMore content here.\r\n";

        let entry = parse_skill_md(content, "web-search");

        assert_eq!(entry.metadata.description, "Search the web for dogs.");
        assert!(entry.parse_status.is_degraded()); // missing frontmatter
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
    fn test_parse_parameters_duplicate_name_is_degraded() {
        let content = r#"---
name: test
description: Test skill
---

## Parameters

- `query` (string, required): The search query
- `query` (boolean, optional): A conflicting redefinition
"#;

        let entry = parse_skill_md(content, "test");

        assert_eq!(entry.parameters.len(), 2); // both entries stay visible
        assert!(
            entry.parse_status.is_degraded(),
            "duplicate parameter name must degrade the entry, got {:?}",
            entry.parse_status
        );
        match &entry.parse_status {
            ParseStatus::Degraded(msg) => assert!(
                msg.contains("duplicate parameter name: query"),
                "issue must name the duplicate, got: {msg}"
            ),
            other => panic!("expected degraded status, got {other:?}"),
        }
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

    #[test]
    fn test_parse_duplicate_parameters_sections_degrade_and_keep_both() {
        let content = r#"---
name: test
description: Test skill
---

## Parameters

- `first` (string, required): From the first section

## Parameters

- `second` (boolean, optional): From the second section
"#;

        let entry = parse_skill_md(content, "test");

        assert!(
            entry.parse_status.is_degraded(),
            "duplicate ## Parameters heading must degrade the entry, got {:?}",
            entry.parse_status
        );
        let names: Vec<&str> = entry.parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["first", "second"],
            "the first section's parameters must not be silently lost"
        );
    }

    #[test]
    fn test_same_h2_under_different_h1_parents_is_ok() {
        // Same-name H2s scoped under different H1 parents are a normal
        // layout, not a duplicate contract section.
        let content = r#"---
name: agentsight
description: Query the agentsight dashboard
---

# Token query

## Common commands

- `tokens` (string, required): list tokens

# Audit query

## Common commands

- `events` (string, required): list events
"#;

        let entry = parse_skill_md(content, "agentsight");

        assert!(
            entry.parse_status.is_ok(),
            "same-name H2 under different H1 parents must parse Ok, got {:?}",
            entry.parse_status
        );
        assert!(entry.parameters.is_empty());
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

    // -----------------------------------------------------------------------
    // Fence Exactness Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_thematic_break_opener_is_not_frontmatter() {
        // A 4-dash horizontal rule is a valid Markdown construct, not a
        // frontmatter fence (the fence is exactly three dashes on their
        // own line, per CommonMark/Jekyll/Hugo and the sec-core scanner).
        // The old prefix match treated it as an opener and dropped
        // everything up to the next `---`-prefixed line out of the body.
        let content = "----\n# My Skill\n\nIntro paragraph.\n\n---\n\nDetails.\n";

        let entry = parse_skill_md(content, "my-skill");

        assert!(entry.parse_status.is_degraded()); // no usable frontmatter
        assert!(
            entry.body.contains("# My Skill")
                && entry.body.contains("Intro paragraph.")
                && entry.body.contains("Details."),
            "a thematic-break opener must not truncate the body: {:?}",
            entry.body
        );
        // The description fallback is derived from the full body now; the
        // old code computed it from the truncated body, whose first
        // paragraph was "Details." (everything before the `---` line had
        // been dropped).
        assert_ne!(
            entry.metadata.description, "Details.",
            "the description fallback must not come from a truncated body"
        );
    }

    #[test]
    fn test_longer_dash_opener_is_not_frontmatter() {
        let content = "-----\nA\n---\nB\n";

        let entry = parse_skill_md(content, "demo");

        assert!(entry.parse_status.is_degraded());
        assert!(
            entry.body.contains('A') && entry.body.contains('B'),
            "body must keep content on both sides of the `---` line: {:?}",
            entry.body
        );
    }

    #[test]
    fn test_closer_requires_exact_fence_line() {
        // A typo'd 4-dash closer is not a fence: the old prefix match
        // landed the body slice on the fourth dash and parsed the skill
        // as Ok with a stray `-` line leaking into the body.
        let content = "---\nname: demo\ndescription: d\n----\n\n# Body\n";

        let entry = parse_skill_md(content, "demo");

        assert!(
            entry.parse_status.is_degraded(),
            "a frontmatter block closed by `----` is unclosed, not Ok: {:?}",
            entry.parse_status
        );
        assert!(
            entry.body.contains("name: demo") && entry.body.contains("# Body"),
            "the whole file must be the body when no exact closer exists: {:?}",
            entry.body
        );
    }

    #[test]
    fn test_closer_with_trailing_space_still_closes() {
        // Trailing whitespace after a fence is an editor artifact, not
        // content: it must not leak into the body.
        let content = "---\nname: demo\ndescription: d\n--- \n\nbody\n";

        let entry = parse_skill_md(content, "demo");

        assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
        assert_eq!(
            entry.body, "\nbody\n",
            "the trailing space must not leak into the body"
        );
    }

    #[test]
    fn test_opener_with_trailing_space_still_opens() {
        let content = "--- \nname: demo\ndescription: d\n---\n\nbody\n";

        let entry = parse_skill_md(content, "demo");

        assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
        assert_eq!(
            entry.body, "\nbody\n",
            "the opener's trailing space must not push the file into the no-frontmatter path"
        );
    }

    #[test]
    fn bare_opener_without_closer_preserves_the_whole_content() {
        // Review regression: a fence-shaped opener with NO closing fence
        // keeps the entire content as the body — including the opener
        // line itself — exactly like the pre-rewrite code (its
        // `after_open.ends_with("---")` arm never matched an empty
        // remainder, so these shapes always fell through to
        // whole-content). Trailing whitespace on the opener and either
        // line ending are covered.
        for content in ["---", "--- \n", "---\r\n"] {
            let entry = parse_skill_md(content, "demo");
            assert!(
                entry.parse_status.is_degraded(),
                "{content:?}: no usable frontmatter"
            );
            assert_eq!(
                entry.body, content,
                "{content:?}: the whole input must stay in the body"
            );
        }
    }

    #[test]
    fn test_body_thematic_break_after_frontmatter_is_preserved() {
        // The closer search stops at the FIRST exact fence line; thematic
        // breaks later in the body are body content and must survive.
        let content = "---\nname: demo\ndescription: d\n---\n\nA\n\n----\n\nB\n";

        let entry = parse_skill_md(content, "demo");

        assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
        assert!(
            entry.body.contains("----"),
            "a body thematic break must be preserved: {:?}",
            entry.body
        );
    }
}
