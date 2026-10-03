//! Directory adapter for the existing Bash/Python code capability.

use super::finding;
use super::input::{Entry, EntryKind, ScanTree};
use crate::{Finding, ScanStatus, SkillSecError, check_deadline};
use asc_capability_code_scan::{Language, Severity};
use serde_json::json;
use std::path::Path;
use std::time::Instant;

const MAX_CODE_BYTES: u64 = 1024 * 1024;

pub(super) fn scan(tree: &ScanTree, deadline: Instant) -> Result<Vec<Finding>, SkillSecError> {
    let mut findings = Vec::new();
    for entry in &tree.entries {
        check_deadline(deadline)?;
        if entry.kind != EntryKind::File {
            continue;
        }
        let path = Path::new(&entry.path);
        let suffix = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !matches!(suffix.as_str(), "" | "sh" | "py") {
            continue;
        }
        let language = match suffix.as_str() {
            "sh" => Some(Language::Bash),
            "py" => Some(Language::Python),
            _ => match tree.read_prefix(entry, deadline) {
                Ok(bytes) => shebang(&bytes),
                Err(SkillSecError::Timeout) => return Err(SkillSecError::Timeout),
                Err(_) => None,
            },
        };
        let Some(language) = language else {
            continue;
        };
        if entry.size > MAX_CODE_BYTES {
            let mut item = error(
                entry,
                language,
                format!(
                    "file too large to scan: {} bytes > {MAX_CODE_BYTES} bytes",
                    entry.size
                ),
            );
            item.metadata
                .insert("max_file_bytes".into(), json!(MAX_CODE_BYTES));
            findings.push(item);
            continue;
        }
        let code = match tree.read(entry, MAX_CODE_BYTES, deadline) {
            Ok(bytes) => {
                if let Ok(text) = String::from_utf8(bytes) {
                    super::text_lines(&text)
                } else {
                    findings.push(error(entry, language, "failed to read file: invalid UTF-8"));
                    continue;
                }
            }
            Err(SkillSecError::Timeout) => return Err(SkillSecError::Timeout),
            Err(error_value) => {
                findings.push(error(
                    entry,
                    language,
                    format!("failed to read file: {error_value}"),
                ));
                continue;
            }
        };
        if code.trim().is_empty() {
            continue;
        }
        let result = asc_capability_code_scan::scan(&code, language, None, "regex");
        check_deadline(deadline)?;
        if !result.ok {
            findings.push(error(entry, language, result.summary));
            continue;
        }
        for item in result.findings {
            let evidence: Vec<_> = item
                .evidence
                .iter()
                .take(5)
                .map(|text| {
                    let mut output: String = text.chars().take(500).collect();
                    if text.chars().count() > 500 {
                        output.push_str("...<truncated>");
                    }
                    output
                })
                .collect();
            findings.push(finding(&item.rule_id, if item.severity == Severity::Deny { ScanStatus::Deny } else { ScanStatus::Warn },
                if item.desc_zh.is_empty() { item.desc_en } else { item.desc_zh }, Some(&entry.path),
                json!({"source":"code-scanner","language":result.language.as_str(),"engine_version":result.engine_version,"elapsed_ms":result.elapsed_ms,"evidence":evidence})));
        }
    }
    Ok(findings)
}

fn shebang(bytes: &[u8]) -> Option<Language> {
    let first = bytes.split(|b| *b == b'\n').next()?;
    let first = first.strip_prefix(b"#!")?;
    for token in String::from_utf8_lossy(first).split_whitespace() {
        let name = Path::new(token).file_name()?.to_str()?.to_lowercase();
        if name.starts_with("python") {
            return Some(Language::Python);
        }
        if matches!(name.as_str(), "sh" | "bash" | "zsh" | "dash") {
            return Some(Language::Bash);
        }
    }
    None
}

fn error(entry: &Entry, language: Language, reason: impl Into<String>) -> Finding {
    finding(
        "code-scanner-error",
        ScanStatus::Warn,
        "code-scanner could not complete this file scan",
        Some(&entry.path),
        json!({"source":"code-scanner","language":language.as_str(),"error":reason.into()}),
    )
}
