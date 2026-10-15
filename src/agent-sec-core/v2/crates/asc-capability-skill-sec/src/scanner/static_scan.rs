//! Embedded Cisco-inspired static rules and Skill package checks.

use super::input::{EntryKind, MAX_BYTES, SKIP, ScanTree};
use super::{finding, truthy};
use crate::{Finding, ScanStatus, SkillSecError, check_deadline};
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
/// How many skipped directories the coverage finding lists before
/// summarizing the remainder as a count.
const LISTED_SKIPPED_DIRS: usize = 16;
/// Extensions that denote private-key material: a file with one of these
/// extensions is a private key by convention (a `ppk` is `PuTTYgen`'s
/// private-key format). They sit outside `TEXT_EXTENSIONS`, so their content
/// is never rule-scanned and without a dedicated finding such a file would
/// pass the static scanner entirely even though it is exactly the credential
/// class the scan exists to catch. Public certificate material (crt/cer/der)
/// is deliberately absent.
const PRIVATE_KEY_EXTENSIONS: &[&str] = &["key", "ppk"];
/// Extensions that may hold private-key material but just as often hold only
/// public certificates: `cert.pem` chains, JKS truststores, Java PKCS#12
/// truststores, and `openssl pkcs12 -export -nokeys` archives share these
/// extensions with their private-key counterparts, so the extension alone
/// cannot establish that the file is a credential. Such files are still
/// reported so they cannot pass the scanner silently, but at medium severity
/// rather than the high that private-key material carries.
const KEYSTORE_EXTENSIONS: &[&str] = &["jks", "keystore", "p12", "pem", "pfx"];

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
) -> Result<Vec<Finding>, SkillSecError> {
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
        if let EntryKind::Special = entry.kind {
            findings.push(item("special-file", "medium", "Skill contains a special file (FIFO, socket or device node); special files are not scanned.", Some(&entry.path), None,
                json!({"category":"filesystem","title":"Special file included","remediation":"Package regular files only; special nodes do not belong in a Skill directory."})));
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
            Err(SkillSecError::Timeout) => return Err(SkillSecError::Timeout),
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
    if !tree.skipped.is_empty() {
        // V2 trust signal: these directories are part of the signed content
        // (content capture excludes only .git/.skill-meta) yet no scanner
        // examines them. Warn, don't deny: dependency directories are
        // legitimate payloads; the operator must decide.
        let mut directories: Vec<&str> = tree.skipped.iter().map(String::as_str).collect();
        directories.sort_unstable();
        let additional = directories.len().saturating_sub(LISTED_SKIPPED_DIRS);
        directories.truncate(LISTED_SKIPPED_DIRS);
        let mut metadata = json!({
            "source": "static-scanner-coverage",
            "category": "scanner_limit",
            "title": "Signed but unscanned directories",
            "remediation": "Remove build output and dependencies from the Skill, or review them manually.",
            "directories": directories,
        });
        if additional > 0 {
            metadata["additional_directories"] = json!(additional);
        }
        findings.push(finding(
            "scan-scope-skip",
            ScanStatus::Warn,
            "Skill contains scanner-excluded directories whose files are still signed and published.",
            None,
            metadata,
        ));
    }
    network(&metadata, &files, &mut findings, deadline)?;
    Ok(findings)
}

fn load_rules() -> Result<Vec<Rule>, SkillSecError> {
    let documents = YamlLoader::load_from_str(include_str!("../../rules/static_rules.yaml"))
        .map_err(|e| SkillSecError::Scanner(e.to_string()))?;
    let list = documents
        .first()
        .and_then(|d| d["rules"].as_vec())
        .ok_or_else(|| SkillSecError::Scanner("invalid static rule resource".into()))?;
    list.iter()
        .map(|rule| {
            let field = |name: &str| {
                rule[name]
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| SkillSecError::Scanner(format!("static rule lacks {name}")))
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

fn regex(pattern: &str) -> Result<Regex, SkillSecError> {
    RegexBuilder::new(pattern)
        .backtrack_limit(32_000_000)
        .build()
        .map_err(|e| SkillSecError::Scanner(e.to_string()))
}

fn read_manifest(
    tree: &ScanTree,
    deadline: Instant,
    findings: &mut Vec<Finding>,
) -> Result<Option<String>, SkillSecError> {
    let entry = tree
        .entries
        .iter()
        .find(|e| e.path == "SKILL.md" && e.kind == EntryKind::File);
    let read = entry.map_or_else(
        || {
            Err(SkillSecError::Scanner(
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
        Err(SkillSecError::Timeout) => Err(SkillSecError::Timeout),
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
    if SECRET_FILES.contains(&filename) {
        findings.push(item("secret-material-file", "high", "Skill contains a file name commonly used for secrets or credentials.", Some(path), None,
            json!({"category":"credential_access","title":"Credential-like file included","remediation":"Remove secrets and credential files from the Skill package."})));
    } else if path.split('/').any(|part| part.starts_with('.')) && path != ".clawhub/origin.json" {
        findings.push(item("hidden-file", "medium", "Skill contains a hidden file or directory.", Some(path), None,
            json!({"category":"filesystem","title":"Hidden file included","remediation":"Keep hidden files out of Skill packages unless they are documented and required."})));
    }
    if PRIVATE_KEY_EXTENSIONS.contains(&extension(path).as_str()) {
        findings.push(item("key-material-file", "high", "Skill contains a file whose extension commonly denotes private-key material.", Some(path), None,
            json!({"category":"credential_access","title":"Private-key material included","remediation":"Remove private keys from the Skill package."})));
    } else if KEYSTORE_EXTENSIONS.contains(&extension(path).as_str()) {
        findings.push(item("keystore-file", "medium", "Skill contains a file whose extension commonly denotes keystore or certificate material that may include private keys.", Some(path), None,
            json!({"category":"credential_access","title":"Keystore or certificate file included","remediation":"Remove keystores and key material from the Skill package unless it is a public certificate."})));
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
    deadline: Instant,
) -> Result<(), SkillSecError> {
    check_deadline(deadline)?;
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
        .map_err(|e| SkillSecError::Scanner(e.to_string()))?
    {
        return Ok(());
    }
    let hint = regex(
        r"(?i)\b(curl|wget)\b|\brequests\.(get|post|put|delete)\s*\(|\burll[iIİı]b\.request\b|\bfetch\s*\(|https?://",
    )?;
    for file in files.iter().filter(|file| file.is_code) {
        let mut block = false;
        for (line, text) in split_lines(&file.text).lines().enumerate() {
            let cleaned = strip_comments(text, &mut block, deadline)?;
            let found = hint
                .find(&cleaned)
                .map_err(|e| SkillSecError::Scanner(e.to_string()))?;
            check_deadline(deadline)?;
            if let Some(found) = found {
                findings.push(item("undeclared-network-access", "medium", "Skill helper content appears to use network access not declared in metadata.", Some(&file.path),
                    Some(u64::try_from(line).unwrap_or(u64::MAX) + 1), json!({"category":"network","title":"Undeclared network behavior",
                    "remediation":"Declare network behavior in SKILL.md metadata or remove the network call.","matchedText":excerpt(found.as_str())})));
                return Ok(());
            }
        }
    }
    Ok(())
}

fn strip_comments(
    text: &str,
    block: &mut bool,
    deadline: Instant,
) -> Result<String, SkillSecError> {
    check_deadline(deadline)?;
    let bytes = text.as_bytes();
    let mut line = String::with_capacity(text.len());
    let (mut cursor, mut start, mut next_check) = (0, 0, 4096);
    let mut replace_block = false;
    // Slice only at ASCII delimiters, preserving UTF-8 and copying each retained span once.
    while cursor + 1 < bytes.len() {
        if cursor >= next_check {
            check_deadline(deadline)?;
            next_check = cursor + 4096;
        }
        if *block && &bytes[cursor..cursor + 2] == b"*/" {
            *block = false;
            cursor += 2;
            start = cursor;
            if replace_block {
                line.push(' ');
                replace_block = false;
            }
        } else if !*block && &bytes[cursor..cursor + 2] == b"/*" {
            line.push_str(&text[start..cursor]);
            *block = true;
            replace_block = true;
            cursor += 2;
        } else {
            cursor += 1;
        }
    }
    check_deadline(deadline)?;
    if *block {
        // An unfinished block preserves the prefix before line-comment stripping, as in V1.
        return Ok(line);
    }
    line.push_str(&text[start..]);
    let (mut cursor, mut next_check) = (0, 4096);
    while cursor < line.len() {
        if cursor >= next_check {
            check_deadline(deadline)?;
            next_check = cursor + 4096;
        }
        let bytes = line.as_bytes();
        if bytes[cursor] == b'#' {
            line.truncate(cursor);
            break;
        }
        if bytes[cursor..].starts_with(b"//") {
            if cursor == 0 || bytes[cursor - 1] != b':' {
                line.truncate(cursor);
                break;
            }
            // Match the previous non-overlapping // search, including the :// exception.
            cursor += 2;
        } else {
            cursor += 1;
        }
    }
    check_deadline(deadline)?;
    Ok(line)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn comments_preserve_v1_prefix_spacing_urls_and_multiline_state() {
        let deadline = Instant::now() + Duration::from_secs(10);
        for (input, expected, in_block, out_block) in [
            ("a/**/b/**/c", "a b c", false, false),
            ("你好/*注释*/世界", "你好 世界", false, false),
            (
                "fetch('https://x') // tail",
                "fetch('https://x') ",
                false,
                false,
            ),
            ("http:///x", "http:///x", false, false),
            ("a # tail /*", "a # tail ", false, true),
            ("a /* open", "a ", false, true),
            ("still open", "", true, true),
            ("end */b/**/c # tail", "b c ", true, false),
            ("/*nested /* inner */ tail */", "  tail */", false, false),
        ] {
            let mut block = in_block;
            assert_eq!(
                strip_comments(input, &mut block, deadline).unwrap(),
                expected,
                "{input}"
            );
            assert_eq!(block, out_block, "{input}");
        }
    }

    #[test]
    fn repeated_short_comments_finish_within_the_scan_deadline() {
        let input = "/**/".repeat(249_999);
        let result =
            strip_comments(&input, &mut false, Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(result, " ".repeat(249_999));
        assert!(matches!(
            strip_comments(&input, &mut false, Instant::now()),
            Err(SkillSecError::Timeout)
        ));
        assert!(matches!(
            network(&json!({}), &[], &mut Vec::new(), Instant::now()),
            Err(SkillSecError::Timeout)
        ));
    }

    fn rules_for(path: &str) -> Vec<String> {
        let mut findings = Vec::new();
        path_findings(path, &mut findings);
        findings.into_iter().map(|finding| finding.rule).collect()
    }

    fn level_for(path: &str, rule: &str) -> Option<ScanStatus> {
        let mut findings = Vec::new();
        path_findings(path, &mut findings);
        findings
            .into_iter()
            .find(|finding| finding.rule == rule)
            .map(|finding| finding.level)
    }

    #[test]
    fn credential_files_are_flagged_without_a_hidden_parent() {
        for path in ["id_rsa", "id_ed25519", "keys/id_rsa", ".env"] {
            assert!(
                rules_for(path).contains(&"secret-material-file".to_owned()),
                "{path}"
            );
        }
        // Hidden-path bookkeeping is unchanged: non-secret dot-paths still warn.
        assert!(rules_for(".ssh/config").contains(&"hidden-file".to_owned()));
        let clawhub = rules_for(".clawhub/origin.json");
        assert!(clawhub.iter().all(|rule| rule != "hidden-file"));
        assert!(rules_for(".clawhub/other.json").contains(&"hidden-file".to_owned()));
    }

    #[test]
    fn private_key_extensions_flag_high_and_keystore_extensions_flag_medium() {
        for path in ["server.key", "identity.ppk", "keys/SERVER.KEY"] {
            assert_eq!(
                level_for(path, "key-material-file"),
                Some(ScanStatus::Deny),
                "{path}"
            );
        }
        // Ambiguous extensions still surface, but only at warn level: a JKS
        // truststore or a cert-only PKCS#12 archive is not credential material.
        for path in [
            "truststore.jks",
            "keys/app.keystore",
            "backup.p12",
            "cert.pem",
            "wallet.pfx",
        ] {
            assert_eq!(
                level_for(path, "keystore-file"),
                Some(ScanStatus::Warn),
                "{path}"
            );
            assert!(
                !rules_for(path).contains(&"key-material-file".to_owned()),
                "{path}"
            );
        }
        for path in [
            "cert.crt",
            "cert.cer",
            "readme.txt",
            "run.sh",
            "archive.tar.gz",
        ] {
            assert!(
                !rules_for(path).contains(&"key-material-file".to_owned()),
                "{path}"
            );
            assert!(
                !rules_for(path).contains(&"keystore-file".to_owned()),
                "{path}"
            );
        }
    }

    #[test]
    fn public_certificates_do_not_trigger_a_high_severity_finding() {
        // A public certificate named cert.pem must not produce a deny-level
        // finding: extension alone cannot tell it from a private-key PEM.
        let mut findings = Vec::new();
        path_findings("cert.pem", &mut findings);
        assert!(
            findings
                .iter()
                .all(|finding| finding.level != ScanStatus::Deny)
        );
        // A certificate-only PFX (`openssl pkcs12 -export -nokeys` produces
        // exactly that shape) is the same ambiguity in PKCS#12 form: it must
        // surface as a warn-level keystore finding, never the deny-level
        // key-material one.
        let mut findings = Vec::new();
        path_findings("public-export.pfx", &mut findings);
        assert!(
            findings
                .iter()
                .all(|finding| finding.level != ScanStatus::Deny),
            "{findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule == "keystore-file")
        );
        // A private key file keeps the high-severity credential finding.
        assert_eq!(
            level_for("server.key", "key-material-file"),
            Some(ScanStatus::Deny)
        );
    }

    #[test]
    fn key_material_extension_matching_is_exact_and_case_insensitive() {
        assert!(rules_for("keys/SERVER.KEY").contains(&"key-material-file".to_owned()));
        assert_eq!(
            level_for("CERT.PEM", "keystore-file"),
            Some(ScanStatus::Warn)
        );
        assert!(!rules_for("server.keyboard").contains(&"key-material-file".to_owned()));
        assert!(!rules_for("monkey").contains(&"key-material-file".to_owned()));
        assert!(!rules_for("monkey").contains(&"keystore-file".to_owned()));
    }
}
