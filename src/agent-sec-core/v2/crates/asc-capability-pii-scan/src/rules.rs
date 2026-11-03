//! Compile a bounded centralized rule document once, without caller HOME state.

use crate::builtin::{BuiltinDetector, PATTERNS, TYPES};
use crate::models::{CustomRuleStatus, CustomRuleSummary, ScanError, Severity};
use crate::scanner::digest;
use fancy_regex::{Regex, RegexBuilder};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use yaml_rust2::parser::{Event, Parser};
use yaml_rust2::{Yaml, YamlLoader};

/// Optional administrator-managed configuration; never resolved relative to HOME.
pub const DEFAULT_CUSTOM_RULES_PATH: &str = "/etc/agent-sec/pii-checker/rules.yaml";
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_RULES: usize = 100;
const MAX_PATTERN_CHARS: usize = 2048;
const MAX_DEPTH: usize = 64;
const BACKTRACK_LIMIT: usize = 1_000_000;

/// Immutable compiled rules shared by concurrent scans through `Arc`.
///
/// Custom configuration is all-or-nothing. Loading failure disables only custom
/// matching and remains visible in every report until a new set is constructed.
pub struct PiiRuleSet {
    pub(crate) builtin: BuiltinDetector,
    pub(crate) custom: Vec<CompiledRule>,
    pub(crate) summary: CustomRuleSummary,
    pub(crate) id: String,
}

pub(crate) struct CompiledRule {
    pub kind: String,
    pub severity: Severity,
    pub pattern: Regex,
}

impl PiiRuleSet {
    /// Builds the shipped rules with an absent custom collection and no file access.
    ///
    /// # Errors
    /// Returns an error only if a shipped builtin pattern cannot compile.
    pub fn builtin() -> Result<Self, ScanError> {
        Self::assemble(Vec::new(), CustomRuleSummary::default())
    }

    /// Loads the centralized default or an explicitly configured absolute path.
    ///
    /// Missing default configuration is absent. Any explicit read failure is
    /// invalid. Updates to the file do not affect an already constructed set.
    ///
    /// # Errors
    /// Returns an error only if a shipped builtin pattern cannot compile.
    pub fn load(path: Option<&Path>) -> Result<Self, ScanError> {
        Self::load_file(
            path.unwrap_or(Path::new(DEFAULT_CUSTOM_RULES_PATH)),
            path.is_none(),
        )
    }

    /// Compiles one UTF-8 YAML document, retaining safe invalid-state diagnostics.
    ///
    /// # Errors
    /// Returns an error only if a shipped builtin pattern cannot compile.
    pub fn from_yaml(content: &[u8]) -> Result<Self, ScanError> {
        if content.len() > MAX_FILE_BYTES {
            return Self::invalid(None, RuleError::FileTooLarge);
        }
        let hash = Some(digest(content));
        let compiled = std::str::from_utf8(content)
            .map_err(|_| RuleError::InvalidUtf8)
            .and_then(compile);
        match compiled {
            Ok(custom) => {
                let summary = CustomRuleSummary {
                    status: CustomRuleStatus::Loaded,
                    rule_count: custom.len(),
                    ruleset_sha256: hash,
                    ..Default::default()
                };
                Self::assemble(custom, summary)
            }
            Err(error) => Self::invalid(hash, error),
        }
    }

    /// Identifies the builtin revision and custom configuration state/content.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns startup state; per-scan counters live only in individual reports.
    pub fn custom_rules(&self) -> &CustomRuleSummary {
        &self.summary
    }

    fn load_file(path: &Path, optional: bool) -> Result<Self, ScanError> {
        if !path.is_absolute() {
            return Self::invalid(None, RuleError::InvalidPath);
        }
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => {
                return Self::builtin();
            }
            Err(_) => return Self::invalid(None, RuleError::ReadError),
        };
        if !metadata.is_file() {
            return Self::invalid(None, RuleError::ReadError);
        }
        let mut content = Vec::new();
        let read = File::open(path).and_then(|file| {
            file.take(u64::try_from(MAX_FILE_BYTES).unwrap_or(u64::MAX) + 1)
                .read_to_end(&mut content)
        });
        if read.is_err() {
            return Self::invalid(None, RuleError::ReadError);
        }
        Self::from_yaml(&content)
    }

    fn invalid(hash: Option<String>, error: RuleError) -> Result<Self, ScanError> {
        Self::assemble(
            Vec::new(),
            CustomRuleSummary {
                status: CustomRuleStatus::Invalid,
                ruleset_sha256: hash,
                error_code: Some(error.to_string()),
                ..Default::default()
            },
        )
    }

    fn assemble(custom: Vec<CompiledRule>, summary: CustomRuleSummary) -> Result<Self, ScanError> {
        let identity = format!(
            "pii-scanner:{}:{PATTERNS}:{}:{:?}:{}:{}",
            crate::SCANNER_VERSION,
            TYPES.join(","),
            summary.status,
            summary.ruleset_sha256.as_deref().unwrap_or(""),
            summary.error_code.as_deref().unwrap_or("")
        );
        Ok(Self {
            builtin: BuiltinDetector::new()?,
            custom,
            summary,
            id: digest(identity),
        })
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
enum RuleError {
    #[error("invalid_path")]
    InvalidPath,
    #[error("read_error")]
    ReadError,
    #[error("file_too_large")]
    FileTooLarge,
    #[error("invalid_utf8")]
    InvalidUtf8,
    #[error("invalid_yaml")]
    InvalidYaml,
    #[error("top_level_not_list")]
    TopLevelNotList,
    #[error("too_many_rules")]
    TooManyRules,
    #[error("invalid_rule_schema")]
    InvalidRuleSchema,
    #[error("invalid_rule_type")]
    InvalidRuleType,
    #[error("duplicate_rule_type")]
    DuplicateRuleType,
    #[error("reserved_rule_type")]
    ReservedRuleType,
    #[error("invalid_regex")]
    InvalidRegex,
    #[error("regex_matches_empty_text")]
    RegexMatchesEmptyText,
}

fn compile(document: &str) -> Result<Vec<CompiledRule>, RuleError> {
    // YamlLoader expands aliases. Reject them and excessive nesting before any
    // tree allocation, so a small file cannot create an unbounded expanded tree.
    let mut parser = Parser::new_from_str(document);
    let mut depth = 0;
    loop {
        match parser.next_token().map_err(|_| RuleError::InvalidYaml)?.0 {
            Event::Alias(_) => return Err(RuleError::InvalidYaml),
            Event::SequenceStart(_, _) | Event::MappingStart(_, _) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(RuleError::InvalidYaml);
                }
            }
            Event::SequenceEnd | Event::MappingEnd => depth -= 1,
            Event::StreamEnd => break,
            _ => {}
        }
    }
    let mut documents = YamlLoader::load_from_str(document).map_err(|_| RuleError::InvalidYaml)?;
    if documents.len() != 1 {
        return Err(RuleError::InvalidYaml);
    }
    let Some(Yaml::Array(items)) = documents.pop() else {
        return Err(RuleError::TopLevelNotList);
    };
    if items.len() > MAX_RULES {
        return Err(RuleError::TooManyRules);
    }
    let mut seen = BTreeSet::new();
    let mut rules = Vec::new();
    for item in items {
        let Yaml::Hash(fields) = item else {
            return Err(RuleError::InvalidRuleSchema);
        };
        if fields
            .keys()
            .any(|k| !matches!(k.as_str(), Some("type" | "regex" | "severity")))
        {
            return Err(RuleError::InvalidRuleSchema);
        }
        let kind = string(&fields, "type")?;
        if kind.len() > 64
            || !kind.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || !kind
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(RuleError::InvalidRuleType);
        }
        if TYPES.contains(&kind) {
            return Err(RuleError::ReservedRuleType);
        }
        if !seen.insert(kind.to_owned()) {
            return Err(RuleError::DuplicateRuleType);
        }
        let expression = string(&fields, "regex")?;
        if expression.chars().count() > MAX_PATTERN_CHARS {
            return Err(RuleError::InvalidRuleSchema);
        }
        let pattern = RegexBuilder::new(expression)
            .backtrack_limit(BACKTRACK_LIMIT)
            .build()
            .map_err(|_| RuleError::InvalidRegex)?;
        if pattern.is_match("").map_err(|_| RuleError::InvalidRegex)? {
            return Err(RuleError::RegexMatchesEmptyText);
        }
        let severity = match fields.get(&Yaml::String("severity".into())) {
            None => Severity::Deny,
            Some(Yaml::String(s)) if s == "deny" => Severity::Deny,
            Some(Yaml::String(s)) if s == "warn" => Severity::Warn,
            _ => return Err(RuleError::InvalidRuleSchema),
        };
        rules.push(CompiledRule {
            kind: kind.into(),
            severity,
            pattern,
        });
    }
    // Preserve document order within each severity, as in V1.
    rules.sort_by_key(|r| r.severity != Severity::Deny);
    Ok(rules)
}

fn string<'a>(fields: &'a yaml_rust2::yaml::Hash, key: &str) -> Result<&'a str, RuleError> {
    fields
        .get(&Yaml::String(key.into()))
        .and_then(Yaml::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(RuleError::InvalidRuleSchema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_parser_handles_literal_character_classes_and_conditionals() {
        for pattern in [r"[(?(]", r"\(\?\(name\)", r"(a)?(?(1)b|c)"] {
            let document = format!("- type: literal\n  regex: '{pattern}'\n");
            let rules = PiiRuleSet::from_yaml(document.as_bytes()).unwrap();
            assert_eq!(rules.summary.status, CustomRuleStatus::Loaded, "{pattern}");
        }
    }

    #[test]
    fn missing_optional_file_differs_from_an_explicit_failure() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("missing.yaml");
        let optional = PiiRuleSet::load_file(&file, true).unwrap();
        assert_eq!(optional.summary.status, CustomRuleStatus::Absent);
        let explicit = PiiRuleSet::load(Some(&file)).unwrap();
        assert_eq!(explicit.summary.status, CustomRuleStatus::Invalid);
        assert_eq!(explicit.summary.error_code.as_deref(), Some("read_error"));
        assert_ne!(optional.id, explicit.id);
    }

    #[test]
    fn pathological_matching_stops_at_the_configured_backtrack_limit() {
        let rules =
            PiiRuleSet::from_yaml(b"- type: expensive\n  regex: '(a|b|ab)*(?>c)'\n").unwrap();
        let input = "ab".repeat(32);
        let result = rules.custom[0].pattern.find(&input);
        assert!(matches!(
            result,
            Err(fancy_regex::Error::RuntimeError(
                fancy_regex::RuntimeError::BacktrackLimitExceeded
            ))
        ));
    }
}
