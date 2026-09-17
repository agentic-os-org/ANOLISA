//! Embedded Cisco-inspired static rules and Skill package checks.

use super::input::{EntryKind, MAX_BYTES, SKIP, ScanTree};
use super::{finding, truthy};
use crate::{Finding, GuardError, ScanStatus, check_deadline};
use fancy_regex::{Regex, RegexBuilder};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use yaml_rust2::YamlLoader;

const CODE_EXTENSIONS: &[&str] = &[
    "bash", "cjs", "js", "mjs", "pl", "ps1", "py", "rb", "sh", "ts", "zsh",
];
const TEXT_EXTENSIONS: &[&str] = &[
    "", "bash", "cfg", "conf", "cjs", "ini", "js", "json", "md", "mjs", "pl", "ps1", "py", "rb",
    "sh", "toml", "ts", "txt", "yaml", "yml", "zsh",
];
const BINARY_EXTENSIONS: &[&str] = &[
    "bin", "class", "dll", "dylib", "exe", "jar", "o", "so", "wasm",
];
const SECRET_FILES: &[&str] = &[
    ".env",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "id_ed25519",
    "id_rsa",
];

struct Rule {
    id: String,
    target: String,
    severity: String,
    category: String,
    title: String,
    message: String,
    remediation: String,
    regex: Regex,
}

struct TextFile {
    path: String,
    text: String,
    is_code: bool,
}

pub(super) fn scan(
    tree: &ScanTree,
    limit: u64,
    deadline: Instant,
) -> Result<Vec<Finding>, GuardError> {
    let rules = load_rules()?;
    let mut findings = Vec::new();
    let skill_text = read_manifest(tree, deadline, &mut findings)?;
    let (metadata, body) = skill_text
        .as_ref()
        .map_or((json!({}), String::new()), |text| {
            manifest(text, &mut findings)
        });
    let mut files = Vec::new();
    let mut entries: Vec<_> = tree.entries.iter().collect();
    entries.sort_by(|a, b| Path::new(&a.path).cmp(Path::new(&b.path)));
    for entry in entries {
        check_deadline(deadline)?;
        if entry.path.split('/').any(|part| SKIP.contains(&part)) {
            continue;
        }
        if let EntryKind::Link { target } = &entry.kind {
            let escapes = target != "inside-root";
            findings.push(item(if escapes {"path-escape-symlink"} else {"symlink-file"}, if escapes {"high"} else {"medium"},
                if escapes {"Skill contains a symlink that resolves outside the Skill directory."} else {"Skill contains a symlink; symlink targets are not scanned."},
                Some(&entry.path), None, json!({"category":if escapes {"path_escape"} else {"filesystem"},
                "title":if escapes {"Symlink target escapes Skill directory"} else {"Symlink skipped"},
                "remediation":"Replace symlinks with regular files inside the Skill directory.","target":target})));
            continue;
        }
        if entry.kind != EntryKind::File {
            continue;
        }
        path_findings(&entry.path, &mut findings);
        if !TEXT_EXTENSIONS.contains(&extension(&entry.path).as_str()) {
            continue;
        }
        if entry.size > limit {
            findings.push(item("large-file-skipped", "medium", "File exceeded static scanner size limit and was skipped.", Some(&entry.path), None,
                json!({"category":"scanner_limit","title":"Large file skipped","remediation":"Keep Skill files small enough for static review or raise the scanner limit.","maxFileBytes":limit})));
            continue;
        }
        let raw = match tree.read(entry, limit, deadline) {
            Ok(raw) => raw,
            Err(GuardError::Timeout) => return Err(GuardError::Timeout),
            Err(error) => {
                findings.push(item("file-read-error", "medium", &format!("File could not be read during static scan: {error}"), Some(&entry.path), None,
                json!({"category":"scanner_error","title":"File read error","remediation":"Ensure the Skill file is readable."})));
                continue;
            }
        };
        if raw.contains(&0) {
            continue;
        }
        match String::from_utf8(raw) {
            Ok(text) => files.push(TextFile { path: entry.path.clone(), is_code: is_code(&entry.path, &text), text }),
            Err(_) => findings.push(item("file-decode-error", "medium", "File is not valid UTF-8 text and could not be scanned.", Some(&entry.path), None,
                json!({"category":"scanner_error","title":"File decode error","remediation":"Store text-like Skill files as UTF-8 text."}))),
        }
    }
    for rule in &rules {
        check_deadline(deadline)?;
        if rule.target == "skill" && skill_text.is_some() {
            apply(rule, "SKILL.md", &body, &mut findings);
        }
        if rule.target == "all_text" || rule.target == "code" {
            for file in &files {
                check_deadline(deadline)?;
                if rule.target == "all_text" || file.is_code {
                    apply(rule, &file.path, &file.text, &mut findings);
                }
            }
        }
    }
    network(&metadata, &files, &mut findings)?;
    Ok(findings)
}

fn load_rules() -> Result<Vec<Rule>, GuardError> {
    let documents = YamlLoader::load_from_str(include_str!("../../rules/static_rules.yaml"))
        .map_err(|e| GuardError::Scanner(e.to_string()))?;
    let list = documents
        .first()
        .and_then(|d| d["rules"].as_vec())
        .ok_or_else(|| GuardError::Scanner("invalid static rule resource".into()))?;
    list.iter()
        .map(|rule| {
            let field = |name: &str| {
                rule[name]
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| GuardError::Scanner(format!("static rule lacks {name}")))
            };
            Ok(Rule {
                id: field("id")?,
                target: field("target")?,
                severity: field("severity")?,
                category: field("category")?,
                title: field("title")?,
                message: field("message")?,
                remediation: field("remediation")?,
                regex: regex(&format!("(?im){}", field("pattern")?))?,
            })
        })
        .collect()
}

fn regex(pattern: &str) -> Result<Regex, GuardError> {
    RegexBuilder::new(pattern)
        .backtrack_limit(32_000_000)
        .build()
        .map_err(|e| GuardError::Scanner(e.to_string()))
}

fn read_manifest(
    tree: &ScanTree,
    deadline: Instant,
    findings: &mut Vec<Finding>,
) -> Result<Option<String>, GuardError> {
    let entry = tree
        .entries
        .iter()
        .find(|e| e.path == "SKILL.md" && e.kind == EntryKind::File);
    let read = entry.map_or_else(
        || {
            Err(GuardError::Scanner(
                "SKILL.md is missing or not a regular file".into(),
            ))
        },
        |e| tree.read(e, MAX_BYTES, deadline),
    );
    match read {
        Ok(bytes) => {
            if let Ok(text) = String::from_utf8(bytes) {
                Ok(Some(super::text_lines(&text)))
            } else {
                findings.push(item("file-decode-error", "medium", "Required file is not valid UTF-8 text.", Some("SKILL.md"), None,
                json!({"category":"scanner_error","title":"File decode error","remediation":"Store SKILL.md as UTF-8 text."})));
                Ok(None)
            }
        }
        Err(GuardError::Timeout) => Err(GuardError::Timeout),
        Err(error) => {
            findings.push(item("file-read-error", "medium", &format!("Required file could not be read: {error}"), Some("SKILL.md"), None,
            json!({"category":"scanner_error","title":"File read error","remediation":"Ensure the Skill file is readable."})));
            Ok(None)
        }
    }
}

fn manifest(text: &str, findings: &mut Vec<Finding>) -> (Value, String) {
    let split_text = split_lines(text);
    let lines: Vec<_> = split_text.lines().collect();
    let mut metadata = json!({});
    let mut body = text.to_owned();
    if lines.first().is_some_and(|line| line.trim() == "---") {
        if let Some(closing) = lines
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, line)| (line.trim() == "---").then_some(i))
        {
            body = lines[closing + 1..].join("\n");
            match super::metadata::parse(&lines[1..closing].join("\n")) {
                Ok(parsed) => {
                    if parsed.is_object() {
                        metadata = parsed;
                    } else if truthy(&parsed) {
                        findings.push(manifest_error(
                            "skill-frontmatter-invalid",
                            "SKILL.md front matter must be a YAML object.",
                            "Invalid Skill metadata",
                            "Use key-value YAML front matter.",
                        ));
                    }
                }
                _ => findings.push(manifest_error(
                    "skill-frontmatter-invalid",
                    "SKILL.md front matter is invalid YAML.",
                    "Invalid Skill metadata",
                    "Fix YAML syntax in SKILL.md front matter.",
                )),
            }
        } else {
            findings.push(manifest_error(
                "skill-frontmatter-unclosed",
                "SKILL.md front matter starts with '---' but has no closing delimiter.",
                "Unclosed Skill metadata",
                "Close YAML front matter with a second '---' line.",
            ));
        }
    } else {
        findings.push(manifest_error(
            "skill-frontmatter-missing",
            "SKILL.md is missing YAML front matter.",
            "Missing Skill metadata",
            "Add YAML front matter with name and description fields.",
        ));
    }
    for key in ["name", "description"] {
        if !metadata.get(key).is_some_and(truthy) {
            findings.push(manifest_error(
                &format!("skill-metadata-missing-{key}"),
                &format!("SKILL.md front matter is missing required field: {key}."),
                "Missing Skill metadata field",
                &format!("Add a non-empty '{key}' field to SKILL.md front matter."),
            ));
        }
    }
    (metadata, body)
}

fn manifest_error(rule: &str, message: &str, title: &str, remediation: &str) -> Finding {
    item(
        rule,
        "medium",
        message,
        Some("SKILL.md"),
        Some(1),
        json!({"category":"manifest","title":title,"remediation":remediation}),
    )
}

fn path_findings(path: &str, findings: &mut Vec<Finding>) {
    let filename = path.rsplit('/').next().unwrap_or(path);
    if path.split('/').any(|part| part.starts_with('.')) {
        if SECRET_FILES.contains(&filename) {
            findings.push(item("secret-material-file", "high", "Skill contains a file name commonly used for secrets or credentials.", Some(path), None,
                json!({"category":"credential_access","title":"Credential-like file included","remediation":"Remove secrets and credential files from the Skill package."})));
        } else if path != ".clawhub/origin.json" {
            findings.push(item("hidden-file", "medium", "Skill contains a hidden file or directory.", Some(path), None,
                json!({"category":"filesystem","title":"Hidden file included","remediation":"Keep hidden files out of Skill packages unless they are documented and required."})));
        }
    }
    if BINARY_EXTENSIONS.contains(&extension(path).as_str()) {
        findings.push(item("suspicious-binary-asset", "medium", "Skill contains a binary executable or bytecode-like asset.", Some(path), None,
            json!({"category":"binary_asset","title":"Suspicious binary asset","remediation":"Remove binary executables or document and verify their provenance."})));
    }
}

fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn is_code(path: &str, text: &str) -> bool {
    if CODE_EXTENSIONS.contains(&extension(path).as_str()) {
        return true;
    }
    let first = split_lines(text)
        .lines()
        .next()
        .unwrap_or("")
        .to_lowercase();
    first.starts_with("#!")
        && ["bash", "sh", "zsh", "python", "node", "ruby", "perl"]
            .iter()
            .any(|m| first.contains(m))
}

fn apply(rule: &Rule, path: &str, text: &str, findings: &mut Vec<Finding>) {
    match rule.regex.find(text) {
        Ok(Some(matched)) => findings.push(item(&rule.id, &rule.severity, &rule.message, Some(path),
            Some(u64::try_from(text[..matched.start()].bytes().filter(|c| *c == b'\n').count()).unwrap_or(u64::MAX) + 1),
            json!({"category":rule.category,"title":rule.title,"remediation":rule.remediation,"matchedText":excerpt(matched.as_str())}))),
        Ok(None) => {},
        Err(error) => findings.push(item("scanner-rule-error", "medium", &format!("Static rule '{}' failed during scan: {error}", rule.id), None, None,
            json!({"category":"scanner_error","title":"Static rule error","remediation":"Fix or disable the failing static rule."}))),
    }
}

fn network(
    metadata: &Value,
    files: &[TextFile],
    findings: &mut Vec<Finding>,
) -> Result<(), GuardError> {
    let declared = [
        "description",
        "allowedTools",
        "allowed_tools",
        "capabilities",
    ]
    .iter()
    .filter_map(|k| metadata.get(k))
    .map(super::scalar_string)
    .collect::<Vec<_>>()
    .join(" ");
    let declaration =
        regex(r"(?i)\b(network|http|https|url|download|fetch|remote|联网|网络|下载|远程)\b")?;
    if declaration
        .is_match(&declared)
        .map_err(|e| GuardError::Scanner(e.to_string()))?
    {
        return Ok(());
    }
    let hint = regex(
        r"(?i)\b(curl|wget)\b|\brequests\.(get|post|put|delete)\s*\(|\burllib\.request\b|\bfetch\s*\(|https?://",
    )?;
    for file in files.iter().filter(|file| file.is_code) {
        let mut block = false;
        for (line, text) in split_lines(&file.text).lines().enumerate() {
            let cleaned = strip_comments(text, &mut block);
            if let Some(found) = hint
                .find(&cleaned)
                .map_err(|e| GuardError::Scanner(e.to_string()))?
            {
                findings.push(item("undeclared-network-access", "medium", "Skill helper content appears to use network access not declared in metadata.", Some(&file.path),
                    Some(u64::try_from(line).unwrap_or(u64::MAX) + 1), json!({"category":"network","title":"Undeclared network behavior",
                    "remediation":"Declare network behavior in SKILL.md metadata or remove the network call.","matchedText":excerpt(found.as_str())})));
                return Ok(());
            }
        }
    }
    Ok(())
}

fn strip_comments(text: &str, block: &mut bool) -> String {
    let mut line = text.to_owned();
    if *block {
        let Some(end) = line.find("*/") else {
            return String::new();
        };
        line = line[end + 2..].to_owned();
        *block = false;
    }
    while let Some(start) = line.find("/*") {
        let Some(end) = line[start + 2..].find("*/").map(|end| start + 2 + end) else {
            *block = true;
            return line[..start].into();
        };
        line = format!("{} {}", &line[..start], &line[end + 2..]);
    }
    let hash = line.find('#');
    let slash = line
        .match_indices("//")
        .find(|(i, _)| *i == 0 || line.as_bytes()[i - 1] != b':')
        .map(|(i, _)| i);
    if let Some(end) = hash.into_iter().chain(slash).min() {
        line.truncate(end);
    }
    line
}

fn excerpt(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= 160 {
        text
    } else {
        format!("{}...", text.chars().take(157).collect::<String>())
    }
}

fn item(
    rule: &str,
    severity: &str,
    message: &str,
    file: Option<&str>,
    line: Option<u64>,
    metadata: Value,
) -> Finding {
    let level = match severity {
        "high" | "critical" => ScanStatus::Deny,
        "medium" | "low" => ScanStatus::Warn,
        _ => ScanStatus::Pass,
    };
    let mut result = finding(
        rule,
        level,
        message,
        file,
        json!({"source":"cisco-skill-scanner-static-only","analyzer":"StaticAnalyzer","severity":severity}),
    );
    result.line = line;
    if let Value::Object(map) = metadata {
        result.metadata.extend(map);
    }
    result
}

fn split_lines(text: &str) -> String {
    text.replace("\r\n", "\n").replace(
        [
            '\r', '\u{000b}', '\u{000c}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}',
            '\u{2028}', '\u{2029}',
        ],
        "\n",
    )
}
