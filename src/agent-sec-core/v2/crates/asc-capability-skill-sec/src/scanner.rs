//! Built-in scanner registry and findings-array import, with no Ledger writes or subprocesses.

mod analyze;
mod code;
mod input;
mod metadata;
mod static_scan;

use crate::{Finding, ScanEntry, ScanStatus, SkillSecError, check_deadline};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

pub use analyze::{AnalyzeResult, analyze};
pub(crate) use input::EntryKind;
pub(crate) use input::ScanTree;

/// Default built-in scanner order retained by scan and baseline operations.
pub const DEFAULT_SCANNERS: [&str; 2] = ["code-scanner", "static-scanner"];
/// Reproducible version of the bundled static rule implementation.
pub const STATIC_VERSION: &str = "cisco-static-only-0.1.1";

/// Registered scanner metadata; reserved invocation types never execute arbitrary commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerConfig {
    /// Canonical public name or a custom external scanner name.
    pub name: String,
    /// Only builtin is executable; skill/cli/api can supply imported findings.
    #[serde(rename = "type", default = "skill_type")]
    pub invocation: String,
    /// Result parser name, defaulting to findings-array.
    #[serde(default = "default_parser")]
    pub parser: String,
    /// Caller-facing registry description.
    #[serde(default)]
    pub description: String,
    /// Disabled scanners are excluded from automatic execution.
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Scanner options, including static maxFileBytes and version metadata.
    #[serde(flatten)]
    pub options: BTreeMap<String, Value>,
}

fn skill_type() -> String {
    "skill".into()
}
fn default_parser() -> String {
    "findings-array".into()
}
const fn enabled() -> bool {
    true
}

/// System-configured scanner registry. Construction performs no I/O or key initialization.
#[derive(Debug, Clone)]
pub struct ScannerRegistry {
    scanners: Vec<ScannerConfig>,
    parsers: BTreeMap<String, String>,
}

impl Default for ScannerRegistry {
    fn default() -> Self {
        let descriptions = [
            ("skill-vetter", "skill", "LLM-driven 4-phase skill audit"),
            (
                "code-scanner",
                "builtin",
                "Scan Skill code files via code-scanner",
            ),
            (
                "static-scanner",
                "builtin",
                "Static Skill security scanner based on Cisco skill-scanner rules",
            ),
        ];
        Self {
            scanners: descriptions
                .into_iter()
                .map(|(name, invocation, description)| ScannerConfig {
                    name: name.into(),
                    invocation: invocation.into(),
                    parser: default_parser(),
                    description: description.into(),
                    enabled: true,
                    options: BTreeMap::new(),
                })
                .collect(),
            parsers: BTreeMap::from([("findings-array".into(), "findings-array".into())]),
        }
    }
}

impl ScannerRegistry {
    /// Merges scanner replacements by name and parser definitions over built-in defaults.
    ///
    /// # Errors
    /// Rejects retired names and invalid static file-size options before any scan or write.
    pub fn new(
        overrides: Vec<ScannerConfig>,
        parsers: BTreeMap<String, String>,
    ) -> Result<Self, SkillSecError> {
        let mut registry = Self::default();
        for scanner in overrides {
            validate_name(&scanner.name)?;
            if scanner.name == "static-scanner" {
                static_limit(&scanner)?;
            }
            if let Some(index) = registry
                .scanners
                .iter()
                .position(|s| s.name == scanner.name)
            {
                registry.scanners[index] = scanner;
            } else {
                registry.scanners.push(scanner);
            }
        }
        registry.parsers.extend(parsers);
        Ok(registry)
    }

    /// Lists configured scanners, including disabled and import-only entries.
    pub fn scanners(&self) -> &[ScannerConfig] {
        &self.scanners
    }

    /// Executes selected enabled built-ins without creating keys, manifests or snapshots.
    ///
    /// # Errors
    /// Rejects invalid names, unreadable trees, expired deadlines and scanner initialization failures.
    pub fn scan(
        &self,
        root: &Path,
        names: Option<&[String]>,
        deadline: Instant,
    ) -> Result<Vec<ScanEntry>, SkillSecError> {
        let requested = requested_names(names)?;
        let tree = ScanTree::open(root, deadline)?;
        self.scan_tree(&tree, &requested, deadline)
    }

    // Callers pass validated selections or built-in constants; raw input enters through scan().
    pub(crate) fn scan_tree(
        &self,
        tree: &ScanTree,
        requested: &[String],
        deadline: Instant,
    ) -> Result<Vec<ScanEntry>, SkillSecError> {
        if !tree.errors.is_empty() {
            return Err(SkillSecError::Scanner(
                "Skill exceeds directory scan limits".into(),
            ));
        }
        let mut results = Vec::new();
        for scanner in &self.scanners {
            check_deadline(deadline)?;
            if !scanner.enabled
                || scanner.invocation != "builtin"
                || !requested.contains(&scanner.name)
            {
                continue;
            }
            let (findings, version) = match scanner.name.as_str() {
                "code-scanner" => (
                    code::scan(tree, deadline)?,
                    scanner
                        .options
                        .get("version")
                        .filter(|v| !v.is_null())
                        .map_or_else(|| env!("CARGO_PKG_VERSION").into(), scalar_string),
                ),
                "static-scanner" => (
                    static_scan::scan(tree, static_limit(scanner)?, deadline)?,
                    STATIC_VERSION.into(),
                ),
                _ => continue,
            };
            check_deadline(deadline)?;
            results.push(scan_entry(scanner.name.clone(), version, findings));
        }
        Ok(results)
    }

    /// Normalizes an external report; no external program or service is invoked.
    ///
    /// # Errors
    /// Rejects retired scanner names, invalid report shape and invalid typed evidence fields.
    pub fn parse_external(
        &self,
        scanner: &str,
        value: &Value,
    ) -> Result<ParsedFindings, SkillSecError> {
        validate_name(scanner)?;
        let mut result = parse_findings(value)?;
        let parser = self
            .scanners
            .iter()
            .find(|s| s.name == scanner)
            .and_then(|s| self.parsers.get(&s.parser));
        if parser.is_some_and(|name| name != "findings-array") {
            result
                .warnings
                .push("Parser is not implemented; using findings-array".into());
        }
        Ok(result)
    }
}

fn static_limit(scanner: &ScannerConfig) -> Result<u64, SkillSecError> {
    scanner
        .options
        .get("maxFileBytes")
        .map_or(Ok(1_000_000), |v| {
            v.as_u64().filter(|n| *n > 0).ok_or_else(|| {
                SkillSecError::Invalid("static maxFileBytes must be a positive integer".into())
            })
        })
}

/// Validates current public scanner input, without historical-name aliases.
///
/// # Errors
/// Rejects empty or retired names; custom scanner names remain supported.
pub fn validate_name(name: &str) -> Result<(), SkillSecError> {
    let replacement = match name {
        "skill-code-scanner" => Some("code-scanner"),
        "cisco-static-scanner" => Some("static-scanner"),
        _ => None,
    };
    if let Some(replacement) = replacement {
        return Err(SkillSecError::Invalid(format!(
            "unsupported scanner name: {name}; use {replacement} instead"
        )));
    }
    if name.trim().is_empty() {
        return Err(SkillSecError::Invalid("scanner name is empty".into()));
    }
    Ok(())
}

pub(crate) fn requested_names(names: Option<&[String]>) -> Result<Vec<String>, SkillSecError> {
    let requested = names.filter(|n| !n.is_empty()).map_or_else(
        || DEFAULT_SCANNERS.map(String::from).to_vec(),
        <[String]>::to_vec,
    );
    for name in &requested {
        validate_name(name)?;
    }
    Ok(requested)
}

pub(crate) fn scan_entry(scanner: String, version: String, findings: Vec<Finding>) -> ScanEntry {
    let status = findings
        .iter()
        .map(|f| f.level)
        .max()
        .unwrap_or(ScanStatus::Pass);
    ScanEntry {
        scanner,
        version,
        status,
        findings,
        scanned_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
    }
}

/// Imported findings with visible normalization warnings, separate from risk findings.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedFindings {
    /// Valid normalized entries.
    pub findings: Vec<Finding>,
    /// Skipped or normalized input diagnostics.
    pub warnings: Vec<String>,
}

fn parse_findings(value: &Value) -> Result<ParsedFindings, SkillSecError> {
    let items = value
        .as_array()
        .or_else(|| value.get("findings").and_then(Value::as_array))
        .ok_or_else(|| {
            SkillSecError::Invalid(
                "expected a JSON array or an object with a 'findings' key".into(),
            )
        })?;
    let mut result = ParsedFindings {
        findings: Vec::new(),
        warnings: Vec::new(),
    };
    for (index, item) in items.iter().enumerate() {
        let Some(item) = item.as_object() else {
            result
                .warnings
                .push(format!("Skipping non-object finding at index {index}"));
            continue;
        };
        let (Some(rule), Some(level)) = (
            item.get("rule").filter(|v| truthy(v)),
            item.get("level").filter(|v| truthy(v)),
        ) else {
            result.warnings.push(format!(
                "Skipping finding at index {index}: missing rule or level"
            ));
            continue;
        };
        let level = match scalar_string(level).to_lowercase().as_str() {
            "pass" => ScanStatus::Pass,
            "deny" => ScanStatus::Deny,
            "warn" => ScanStatus::Warn,
            _ => {
                result
                    .warnings
                    .push(format!("Unknown level at index {index}; treating as warn"));
                ScanStatus::Warn
            }
        };
        let mut metadata: BTreeMap<String, Value> = match item.get("metadata") {
            None => BTreeMap::new(),
            Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            _ => {
                return Err(SkillSecError::Invalid(
                    "finding metadata must be an object".into(),
                ));
            }
        };
        metadata.extend(
            item.iter()
                .filter(|(k, _)| {
                    !["rule", "level", "message", "file", "line", "metadata"].contains(&k.as_str())
                })
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        let file = item
            .get("file")
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .ok_or_else(|| SkillSecError::Invalid("finding file must be a string".into()))
            })
            .transpose()?;
        let line = item
            .get("line")
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|v| v.parse().ok()))
                    .ok_or_else(|| {
                        SkillSecError::Invalid("finding line must be nonnegative".into())
                    })
            })
            .transpose()?;
        result.findings.push(Finding {
            rule: scalar_string(rule),
            level,
            message: item.get("message").map_or_else(String::new, scalar_string),
            file,
            line,
            metadata,
        });
    }
    Ok(result)
}

fn scalar_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => value.to_string(),
    }
}

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}

pub(crate) fn finding(
    rule: &str,
    level: ScanStatus,
    message: impl Into<String>,
    file: Option<&str>,
    metadata: Value,
) -> Finding {
    Finding {
        rule: rule.into(),
        level,
        message: message.into(),
        file: file.map(String::from),
        line: None,
        metadata: match metadata {
            Value::Object(map) => map.into_iter().collect(),
            _ => BTreeMap::new(),
        },
    }
}

pub(crate) fn error_finding(rule: &str, message: impl Into<String>, file: &str) -> Finding {
    finding(rule, ScanStatus::Warn, message, Some(file), json!({}))
}

// Python's text-file reads normalize universal newlines before rule matching.
fn text_lines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
