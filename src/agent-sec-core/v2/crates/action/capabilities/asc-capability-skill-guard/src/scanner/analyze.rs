//! Read-only analysis preserves risk verdicts separately from coverage and execution failures.

use super::{ScanTree, ScannerRegistry, error_finding, input::EntryKind};
use crate::filesystem::open_directory;
use crate::{Finding, GuardError, ScanStatus, check_deadline};
use rustix::fs::{AtFlags, FileType, statat};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;

const ERROR_RULES: &[&str] = &[
    "code-scanner-error",
    "file-decode-error",
    "file-read-error",
    "large-file-skipped",
    "scanner-rule-error",
];

/// Analyze business output and process exit status; no Ledger state is touched.
#[derive(Debug, Clone, Serialize)]
pub struct AnalyzeResult {
    /// Stable schema-version-one consumer object.
    pub data: Value,
    /// Zero for completed analysis (including deny), one for incomplete coverage, two for bad input.
    pub exit_code: i32,
}

/// Scans the current directory independently of signing keys and managed-directory registration.
///
/// # Errors
/// A consumed deadline remains an execution failure, never a completed risk verdict.
pub fn analyze(root: &Path, deadline: Instant) -> Result<AnalyzeResult, GuardError> {
    let invalid = match root.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            Some(("root-symlink", "Skill root must not be a symbolic link."))
        }
        Ok(m) if !m.is_dir() => Some(("root-not-directory", "Skill root is not a directory.")),
        Err(_) => Some(("root-not-found", "Skill root does not exist.")),
        _ => None,
    };
    if let Some((code, message)) = invalid {
        return Ok(failure(vec![json!({"code":code,"message":message})], 2));
    }
    let tree = match open_tree(root, deadline) {
        Ok(tree) => tree,
        Err(GuardError::Timeout) => return Err(GuardError::Timeout),
        Err(_) => {
            return Ok(failure(
                vec![
                    json!({"code":"directory-read-error","message":"Skill directory is not readable."}),
                ],
                1,
            ));
        }
    };
    let Some(tree) = tree.filter(|tree| {
        !tree.errors.is_empty()
            || tree
                .entries
                .iter()
                .any(|e| e.path == "SKILL.md" && e.kind == EntryKind::File)
    }) else {
        return Ok(failure(
            vec![
                json!({"code":"skill-manifest-missing","message":"Skill root must contain a regular SKILL.md file.","file":"SKILL.md"}),
            ],
            2,
        ));
    };
    let mut errors = tree.errors.clone();
    errors.extend(tree.entries.iter().filter(|e| e.kind == EntryKind::Special).map(|e| json!({"code":"unsupported-file-type","message":"Skill contains a non-regular file that cannot be scanned.","file":e.path})));
    if !errors.is_empty() {
        sort_errors(&mut errors);
        return Ok(failure(errors, 1));
    }
    let registry = ScannerRegistry::default();
    let mut results = Vec::new();
    for name in super::DEFAULT_SCANNERS {
        let names = [name.to_owned()];
        let scan = registry.scan_tree(&tree, Some(&names), deadline);
        let (version, findings) = match scan {
            Ok(mut scans) => {
                let entry = scans
                    .pop()
                    .ok_or_else(|| GuardError::Scanner("default scanner did not run".into()))?;
                (entry.version, entry.findings)
            }
            Err(GuardError::Timeout) => return Err(GuardError::Timeout),
            Err(_) => (
                "unknown".into(),
                vec![error_finding(
                    "scanner-error",
                    format!("{name} failed to complete."),
                    "",
                )],
            ),
        };
        results.push(scanner_result(name, &version, findings));
    }
    let complete = results.iter().all(|r| r["coverage_complete"] == true);
    let status = if complete {
        results
            .iter()
            .map(|r| r["status"].as_str().unwrap_or("pass"))
            .max_by_key(|s| match *s {
                "deny" => 2,
                "warn" => 1,
                _ => 0,
            })
            .unwrap_or("pass")
    } else {
        "error"
    };
    let mut data = json!({"schema_version":"1","engine_version":env!("CARGO_PKG_VERSION"),"status":status,"coverage_complete":complete,"scanners":results,"errors":[]});
    sanitize(&mut data, &sensitive_roots(root));
    Ok(AnalyzeResult {
        data,
        exit_code: i32::from(!complete),
    })
}

fn open_tree(root: &Path, deadline: Instant) -> Result<Option<ScanTree>, GuardError> {
    check_deadline(deadline)?;
    let directory = open_directory(root)?;
    match statat(&directory, "SKILL.md", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {}
        Ok(_) | Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(crate::io_error(root.join("SKILL.md"), error)),
    }
    ScanTree::from_directory(directory, root, deadline).map(Some)
}

fn scanner_result(name: &str, version: &str, raw: Vec<Finding>) -> Value {
    let mut findings = Vec::new();
    let mut errors = Vec::new();
    for mut finding in raw {
        if ERROR_RULES.contains(&finding.rule.as_str()) || finding.rule == "scanner-error" {
            finding.metadata.remove("error");
            let mut error = json!({"code":finding.rule,"message":finding.message});
            if let Some(path) = finding.file.filter(|s| !s.is_empty()) {
                error["file"] = json!(path);
            }
            if !finding.metadata.is_empty() {
                error["metadata"] = json!(finding.metadata);
            }
            errors.push(error);
        } else {
            findings.push(finding);
        }
    }
    findings.sort_by(|a, b| {
        (&a.file, a.line.unwrap_or(0), &a.rule).cmp(&(&b.file, b.line.unwrap_or(0), &b.rule))
    });
    sort_errors(&mut errors);
    let complete = errors.is_empty();
    let status = if complete {
        match findings
            .iter()
            .map(|f| f.level)
            .max()
            .unwrap_or(ScanStatus::Pass)
        {
            ScanStatus::None => "none",
            ScanStatus::Pass => "pass",
            ScanStatus::Warn => "warn",
            ScanStatus::Deny => "deny",
        }
    } else {
        "error"
    };
    json!({"name":name,"version":version,"status":status,"coverage_complete":complete,"findings":findings,"errors":errors})
}

fn sort_errors(errors: &mut [Value]) {
    errors.sort_by(|a, b| {
        (
            a["file"].as_str().unwrap_or(""),
            a["code"].as_str().unwrap_or(""),
        )
            .cmp(&(
                b["file"].as_str().unwrap_or(""),
                b["code"].as_str().unwrap_or(""),
            ))
    });
}

fn failure(errors: Vec<Value>, exit_code: i32) -> AnalyzeResult {
    AnalyzeResult {
        data: json!({"schema_version":"1","engine_version":env!("CARGO_PKG_VERSION"),"status":"error","coverage_complete":false,"scanners":[],"errors":Value::Array(errors)}),
        exit_code,
    }
}

fn sensitive_roots(root: &Path) -> Vec<String> {
    let mut roots: Vec<_> = [root.to_path_buf(), std::env::temp_dir()]
        .into_iter()
        .chain(
            [
                "HOME",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_CACHE_HOME",
                "TMPDIR",
                "TEMP",
                "TMP",
            ]
            .iter()
            .filter_map(std::env::var_os)
            .map(std::path::PathBuf::from),
        )
        .filter(|p| p.is_absolute() && p.parent().is_some())
        .filter_map(|p| p.to_str().map(String::from))
        .collect();
    roots.sort_by_key(|s| std::cmp::Reverse(s.len()));
    roots.dedup();
    roots
}

fn sanitize(value: &mut Value, roots: &[String]) {
    match value {
        Value::String(text) => {
            for root in roots {
                *text = text.replace(root, "<redacted>");
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize(item, roots);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                sanitize(item, roots);
            }
        }
        _ => {}
    }
}
