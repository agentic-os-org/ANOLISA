//! Loading of the shipped Bash and Python rule sets.
//!
//! V1 reads the YAML from disk next to the installed Python package. The V2
//! scanner runs inside `agent-sec-daemon`, whose RPM ships the binary alone, so
//! the documents are embedded with `include_str!` instead. Rules therefore
//! change only with a rebuild and a daemon restart.
//!
//! The tables below are sorted by file name, which is not cosmetic: it decides
//! the order of findings and therefore the rule-id list inside the result
//! summary. Files starting with `_` hold shared data rather than rules.

use std::collections::BTreeMap;

use yaml_rust2::{Yaml, YamlLoader};

use crate::errors::CodeScanError;

/// A language whose rules ship with this crate.
///
/// Kept local until the shared Action contract crate lands and owns the
/// wire-facing enum. Serializes to the lowercase name V1 emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Shell code, and the only entry point for inline extraction.
    Bash,
    /// Python code.
    Python,
}

impl Language {
    /// Returns the wire and directory name used by both V1 and V2.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Python => "python",
        }
    }

    /// Parses a language name.
    ///
    /// # Errors
    /// Returns [`CodeScanError::UnsupportedLanguage`] for anything else,
    /// including a different case: V1 matches these names exactly.
    pub fn parse(value: &str) -> Result<Self, CodeScanError> {
        match value {
            "bash" => Ok(Self::Bash),
            "python" => Ok(Self::Python),
            other => Err(CodeScanError::UnsupportedLanguage(other.to_owned())),
        }
    }
}

/// How seriously a matching rule treats its finding.
///
/// Every shipped rule is `Warn` today. `Deny` exists because the verdict
/// aggregation contract distinguishes the two and rules may adopt it later.
/// Serializes to the lowercase name V1 emits inside a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Report the finding without asking the caller to block.
    Warn,
    /// Report the finding and ask the caller to block.
    Deny,
}

impl Severity {
    /// Returns the wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Deny => "deny",
        }
    }
}

/// One rule as authored in YAML, after reference resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDefinition {
    /// Stable identifier reported in findings.
    pub rule_id: String,
    /// Weakness classification, carried for reporting only.
    pub cwe_id: String,
    /// English description.
    pub desc_en: String,
    /// Chinese description.
    pub desc_zh: String,
    /// Main pattern, with authoring newlines already removed.
    pub regex: String,
    /// Severity contributed to the verdict when this rule matches.
    pub severity: Severity,
    /// Optional patterns that switch this rule to segment-level matching.
    pub target_regexes: Option<Vec<String>>,
}

/// `(file stem, document)` for every Bash YAML file, in file-name order.
const BASH_DOCUMENTS: &[(&str, &str)] = &[
    ("_shared", include_str!("../rules/bash/_shared.yaml")),
    (
        "shell-alias-injection",
        include_str!("../rules/bash/shell-alias-injection.yaml"),
    ),
    (
        "shell-archive-unsafe-extract",
        include_str!("../rules/bash/shell-archive-unsafe-extract.yaml"),
    ),
    (
        "shell-cd-sensitive-dir",
        include_str!("../rules/bash/shell-cd-sensitive-dir.yaml"),
    ),
    (
        "shell-cmd-subshell-exec",
        include_str!("../rules/bash/shell-cmd-subshell-exec.yaml"),
    ),
    (
        "shell-dangerous-permission",
        include_str!("../rules/bash/shell-dangerous-permission.yaml"),
    ),
    (
        "shell-data-exfil",
        include_str!("../rules/bash/shell-data-exfil.yaml"),
    ),
    (
        "shell-disk-wipe",
        include_str!("../rules/bash/shell-disk-wipe.yaml"),
    ),
    (
        "shell-download-exec",
        include_str!("../rules/bash/shell-download-exec.yaml"),
    ),
    (
        "shell-find-delete",
        include_str!("../rules/bash/shell-find-delete.yaml"),
    ),
    (
        "shell-git-http-clone",
        include_str!("../rules/bash/shell-git-http-clone.yaml"),
    ),
    (
        "shell-git-ssl-bypass",
        include_str!("../rules/bash/shell-git-ssl-bypass.yaml"),
    ),
    (
        "shell-kernel-module",
        include_str!("../rules/bash/shell-kernel-module.yaml"),
    ),
    (
        "shell-obfuscation",
        include_str!("../rules/bash/shell-obfuscation.yaml"),
    ),
    (
        "shell-passwd-useradd",
        include_str!("../rules/bash/shell-passwd-useradd.yaml"),
    ),
    (
        "shell-persistence",
        include_str!("../rules/bash/shell-persistence.yaml"),
    ),
    (
        "shell-pkg-integrity-bypass",
        include_str!("../rules/bash/shell-pkg-integrity-bypass.yaml"),
    ),
    (
        "shell-pkg-tls-bypass",
        include_str!("../rules/bash/shell-pkg-tls-bypass.yaml"),
    ),
    (
        "shell-read-sensitive-file",
        include_str!("../rules/bash/shell-read-sensitive-file.yaml"),
    ),
    (
        "shell-recursive-delete",
        include_str!("../rules/bash/shell-recursive-delete.yaml"),
    ),
    (
        "shell-reverse-shell",
        include_str!("../rules/bash/shell-reverse-shell.yaml"),
    ),
    (
        "shell-security-disable",
        include_str!("../rules/bash/shell-security-disable.yaml"),
    ),
    (
        "shell-self-protect-hermes",
        include_str!("../rules/bash/shell-self-protect-hermes.yaml"),
    ),
    (
        "shell-self-protect-openclaw",
        include_str!("../rules/bash/shell-self-protect-openclaw.yaml"),
    ),
    (
        "shell-ssh-keygen-weak",
        include_str!("../rules/bash/shell-ssh-keygen-weak.yaml"),
    ),
    (
        "shell-system-file-delete",
        include_str!("../rules/bash/shell-system-file-delete.yaml"),
    ),
    (
        "shell-tamper-sensitive-file",
        include_str!("../rules/bash/shell-tamper-sensitive-file.yaml"),
    ),
];

/// `(file stem, document)` for every Python YAML file, in file-name order.
const PYTHON_DOCUMENTS: &[(&str, &str)] = &[
    ("_shared", include_str!("../rules/python/_shared.yaml")),
    (
        "py-data-exfil",
        include_str!("../rules/python/py-data-exfil.yaml"),
    ),
    (
        "py-download-exec",
        include_str!("../rules/python/py-download-exec.yaml"),
    ),
    (
        "py-obfuscation",
        include_str!("../rules/python/py-obfuscation.yaml"),
    ),
    (
        "py-recursive-delete",
        include_str!("../rules/python/py-recursive-delete.yaml"),
    ),
    (
        "py-reverse-shell",
        include_str!("../rules/python/py-reverse-shell.yaml"),
    ),
    (
        "py-sensitive-file-access",
        include_str!("../rules/python/py-sensitive-file-access.yaml"),
    ),
    (
        "py-tls-bypass",
        include_str!("../rules/python/py-tls-bypass.yaml"),
    ),
    (
        "py-unsafe-deserialization",
        include_str!("../rules/python/py-unsafe-deserialization.yaml"),
    ),
    (
        "py-weak-crypto",
        include_str!("../rules/python/py-weak-crypto.yaml"),
    ),
];

/// File stem holding shared `target_regexes_ref` lists.
const SHARED_STEM: &str = "_shared";

/// Returns every rule for `language`, in file-name order.
///
/// # Errors
/// Returns the first rule-layer failure encountered: an unparsable document, an
/// unresolvable `target_regexes_ref`, or a document failing field validation.
pub fn load_rules(language: Language) -> Result<Vec<RuleDefinition>, CodeScanError> {
    let documents = match language {
        Language::Bash => BASH_DOCUMENTS,
        Language::Python => PYTHON_DOCUMENTS,
    };
    let shared = load_shared(documents)?;
    let mut rules = Vec::with_capacity(documents.len());
    for (stem, document) in documents {
        if stem.starts_with('_') {
            continue;
        }
        rules.push(parse_rule(stem, document, &shared)?);
    }
    Ok(rules)
}

/// Reads `_shared.yaml` into the map consulted by `target_regexes_ref`.
///
/// A shared document that is absent, empty or not a mapping yields an empty
/// map, mirroring V1: a rule referencing it then fails with a resolve error at
/// its own file rather than here.
fn load_shared(documents: &[(&str, &str)]) -> Result<BTreeMap<String, Vec<String>>, CodeScanError> {
    let Some((_, document)) = documents.iter().find(|(stem, _)| *stem == SHARED_STEM) else {
        return Ok(BTreeMap::new());
    };
    let Some(root) = load_document(document, SHARED_STEM)? else {
        return Ok(BTreeMap::new());
    };
    let Yaml::Hash(entries) = root else {
        return Ok(BTreeMap::new());
    };
    let mut shared = BTreeMap::new();
    for (key, value) in entries {
        let (Yaml::String(name), Yaml::Array(items)) = (key, value) else {
            continue;
        };
        let patterns = items
            .into_iter()
            .filter_map(|item| match item {
                Yaml::String(pattern) => Some(pattern),
                _ => None,
            })
            .collect();
        shared.insert(name, patterns);
    }
    Ok(shared)
}

/// Parses one rule document and resolves its shared reference.
///
/// # Errors
/// Returns a parse, resolve or validation failure naming `stem`.
fn parse_rule(
    stem: &str,
    document: &str,
    shared: &BTreeMap<String, Vec<String>>,
) -> Result<RuleDefinition, CodeScanError> {
    let root = load_document(document, stem)?
        .ok_or_else(|| CodeScanError::RuleValidation(stem.to_owned()))?;
    let Yaml::Hash(fields) = root else {
        return Err(CodeScanError::RuleValidation(stem.to_owned()));
    };

    let target_regexes = resolve_targets(&fields, stem, shared)?;
    let severity = match required_string(&fields, "severity", stem)?.as_str() {
        "warn" => Severity::Warn,
        "deny" => Severity::Deny,
        _ => return Err(CodeScanError::RuleValidation(stem.to_owned())),
    };

    // V1 strips authoring newlines so a block scalar reads as one pattern.
    let regex = required_string(&fields, "regex", stem)?.replace('\n', "");
    reject_named_conditionals(&regex, stem)?;
    for target in target_regexes.iter().flatten() {
        reject_named_conditionals(target, stem)?;
    }

    Ok(RuleDefinition {
        rule_id: required_string(&fields, "rule_id", stem)?,
        cwe_id: required_string(&fields, "cwe_id", stem)?,
        desc_en: required_string(&fields, "desc_en", stem)?,
        desc_zh: required_string(&fields, "desc_zh", stem)?,
        regex,
        severity,
        target_regexes,
    })
}

/// Rejects conditional groups whose condition names a group, as in `(?(q)x)`.
///
/// This is the one Python construct fancy-regex accepts and then misreads: it
/// compiles the pattern but never takes the branch, so the rule silently stops
/// reporting what it was written to report. Numeric conditions such as `(?(1)x)`
/// are understood identically by both engines and stay allowed.
///
/// Detection is a literal scan, so a `(?(` written as escaped literal text would
/// be rejected too. No shipped rule does that, and refusing to load is the safe
/// direction for a security rule set.
///
/// # Errors
/// Returns [`CodeScanError::RegexCompile`] naming `stem`. The variant is V1's
/// existing "pattern unusable by the engine" failure; adding a code of our own
/// would change an error contract that end-to-end goldens assert.
fn reject_named_conditionals(pattern: &str, stem: &str) -> Result<(), CodeScanError> {
    let mut rest = pattern;
    while let Some(offset) = rest.find("(?(") {
        rest = &rest[offset + 3..];
        if !rest.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(CodeScanError::RegexCompile(stem.to_owned()));
        }
    }
    Ok(())
}

/// Resolves `target_regexes` either inline or through `target_regexes_ref`.
///
/// # Errors
/// Returns [`CodeScanError::RuleRefResolve`] when the referenced name is absent
/// from the shared document, and [`CodeScanError::RuleValidation`] when either
/// field carries a non-list or a list of non-strings.
fn resolve_targets(
    fields: &yaml_rust2::yaml::Hash,
    stem: &str,
    shared: &BTreeMap<String, Vec<String>>,
) -> Result<Option<Vec<String>>, CodeScanError> {
    if let Some(reference) = fields.get(&Yaml::String("target_regexes_ref".to_owned())) {
        let Yaml::String(name) = reference else {
            return Err(CodeScanError::RuleValidation(stem.to_owned()));
        };
        return shared
            .get(name)
            .cloned()
            .map(Some)
            .ok_or_else(|| CodeScanError::RuleRefResolve(stem.to_owned()));
    }
    match fields.get(&Yaml::String("target_regexes".to_owned())) {
        None | Some(Yaml::Null) => Ok(None),
        Some(Yaml::Array(items)) => items
            .iter()
            .map(|item| match item {
                Yaml::String(pattern) => Ok(pattern.clone()),
                _ => Err(CodeScanError::RuleValidation(stem.to_owned())),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(CodeScanError::RuleValidation(stem.to_owned())),
    }
}

/// Reads a required string field.
///
/// # Errors
/// Returns [`CodeScanError::RuleValidation`] when the field is missing, not a
/// string, or empty. V1 relies on pydantic for the first two and on the regex
/// engine for the third; rejecting empties here keeps a blank pattern from
/// matching every input.
fn required_string(
    fields: &yaml_rust2::yaml::Hash,
    field: &str,
    stem: &str,
) -> Result<String, CodeScanError> {
    match fields.get(&Yaml::String(field.to_owned())) {
        Some(Yaml::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(CodeScanError::RuleValidation(stem.to_owned())),
    }
}

/// Loads a single YAML document, returning `None` for an empty file.
///
/// # Errors
/// Returns [`CodeScanError::RuleYamlParse`] when the document is malformed or
/// the file carries more than one document.
fn load_document(document: &str, stem: &str) -> Result<Option<Yaml>, CodeScanError> {
    let mut documents = YamlLoader::load_from_str(document)
        .map_err(|_| CodeScanError::RuleYamlParse(stem.to_owned()))?;
    if documents.len() > 1 {
        return Err(CodeScanError::RuleYamlParse(stem.to_owned()));
    }
    Ok(documents.pop().filter(|value| !matches!(value, Yaml::Null)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_conditions_are_rejected() {
        for pattern in [
            r#"a=(?P<q>["'])?x(?(q)(?P=q))"#,
            r"(?(name)then|else)",
            // Rejected at the second occurrence, after an allowed numeric one.
            r"(?(1)\1)(?(q)x)",
        ] {
            assert_eq!(
                reject_named_conditionals(pattern, "probe"),
                Err(CodeScanError::RegexCompile("probe".to_owned())),
                "accepted a named condition: {pattern}"
            );
        }
    }

    #[test]
    fn numeric_conditions_and_ordinary_patterns_are_allowed() {
        for pattern in [
            r#"a=(["'])?x(?(1)\1)"#,
            r"(?(12)x)",
            // Lookaround and non-capturing groups must not trip the scan.
            r"\bdd\b(?:x|y)(?=$|[\s;])(?!\()",
            "",
        ] {
            assert_eq!(
                reject_named_conditionals(pattern, "probe"),
                Ok(()),
                "rejected a supported pattern: {pattern}"
            );
        }
    }

    #[test]
    fn shipped_rules_pass_the_guard() {
        // The V2 copy of shell-disk-wipe carries the numeric form on purpose.
        // If it is ever synced back from V1, this fails before release.
        for language in [Language::Bash, Language::Python] {
            assert!(load_rules(language).is_ok(), "{}", language.as_str());
        }
    }
}
