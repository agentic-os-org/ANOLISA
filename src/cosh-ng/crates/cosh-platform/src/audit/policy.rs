//! Policy loading and validation.
//!
//! Resolves the active policy from (in priority order):
//! 1. `$COSH_AUDIT_POLICY`           (explicit override; CI/tests)
//! 2. `~/.copilot-shell/cosh/audit.toml` (per-user)
//! 3. `/etc/cosh/audit.toml`         (system; ops-managed)
//! 4. Built-in `balanced` preset     (factory default)
//!
//! Only the first existing source is used — no merging across files. See
//! `docs/audit-design.md` §6 ("avoid multi-file unpredictable decisions").

use std::path::{Path, PathBuf};

use cosh_types::audit::{Match, Policy, StringMatch};
use cosh_types::error::{CoshError, ErrorCode};
use sha2::{Digest, Sha256};

use super::builtin::{self, BuiltinPreset};

/// A policy that has been loaded from some source, validated, and tagged
/// with an audit-traceable `policy_version` string. The `policy_version`
/// is what gets recorded in every `Decision` so that any audit log entry
/// can be mapped back to a specific policy revision.
#[derive(Debug, Clone)]
pub struct LoadedPolicy {
    pub policy: Policy,
    pub source: PolicySource,
    pub policy_version: String,
}

#[derive(Debug, Clone)]
pub enum PolicySource {
    UserFile(PathBuf),
    Builtin(BuiltinPreset),
}

impl PolicySource {
    pub fn label(&self) -> String {
        match self {
            PolicySource::UserFile(p) => format!("file:{}", p.display()),
            PolicySource::Builtin(p) => format!("builtin:{}", p.name()),
        }
    }
}

impl LoadedPolicy {
    /// Resolve and load the active policy. Returns the loaded policy plus
    /// an optional warning string (e.g. when a configured user file failed
    /// to load and we fell back to the built-in `balanced` preset).
    pub fn load() -> (LoadedPolicy, Option<String>) {
        for path in candidate_policy_paths() {
            if path.exists() {
                match LoadedPolicy::from_user_file(&path) {
                    Ok(loaded) => return (loaded, None),
                    Err(e) => {
                        let warn = format!(
                            "policy file {} failed to load ({}); falling back to builtin balanced",
                            path.display(),
                            e.message
                        );
                        return (builtin::balanced(), Some(warn));
                    }
                }
            }
        }
        (builtin::balanced(), None)
    }

    /// Load from a user-controlled TOML file on disk.
    pub fn from_user_file(path: &Path) -> Result<LoadedPolicy, CoshError> {
        let bytes = std::fs::read(path).map_err(|e| {
            CoshError::new(
                ErrorCode::AuditPolicyError,
                format!("failed to read {}: {}", path.display(), e),
                "audit",
            )
            .with_hint("check file permissions or remove $COSH_AUDIT_POLICY")
        })?;
        let policy = parse_and_validate(&bytes).map_err(|msg| {
            CoshError::new(
                ErrorCode::AuditPolicyError,
                format!("invalid policy at {}: {}", path.display(), msg),
                "audit",
            )
            .with_hint("see docs/audit-design.md §6 for valid policy syntax")
        })?;
        let hash = sha256_hex(&bytes);
        let policy_version = format!("user@{}+sha256:{}", policy.version, hash);
        Ok(LoadedPolicy {
            policy,
            source: PolicySource::UserFile(path.to_path_buf()),
            policy_version,
        })
    }

    /// Construct from an embedded built-in preset (TOML source compiled
    /// into the binary). The hash of the TOML source bytes is part of
    /// `policy_version` so that a release with mismatched bundled source
    /// is detectable in the audit log.
    pub fn from_builtin(preset: BuiltinPreset, source_toml: &str) -> LoadedPolicy {
        // Built-in TOML is authored by us — parse failure is a build-time
        // bug, not a runtime condition. Panic on it so it's caught in CI.
        let policy = parse_and_validate(source_toml.as_bytes())
            .expect("built-in audit policy TOML must parse and validate");
        let hash = sha256_hex(source_toml.as_bytes());
        let policy_version = format!(
            "builtin-{}@{}+sha256:{}",
            preset.name(),
            env!("CARGO_PKG_VERSION"),
            hash
        );
        LoadedPolicy {
            policy,
            source: PolicySource::Builtin(preset),
            policy_version,
        }
    }
}

/// Public TOML validation entry point — used by `cosh audit policy validate`.
pub fn validate_toml_bytes(bytes: &[u8]) -> Result<Policy, String> {
    parse_and_validate(bytes)
}

fn parse_and_validate(bytes: &[u8]) -> Result<Policy, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("not UTF-8: {}", e))?;
    let policy: Policy = toml::from_str(text).map_err(|e| format!("toml parse error: {}", e))?;
    validate_policy(&policy)?;
    Ok(policy)
}

fn validate_policy(policy: &Policy) -> Result<(), String> {
    for (idx, rule) in policy.rules.iter().enumerate() {
        if rule.name.trim().is_empty() {
            return Err(format!("rule[{}] has empty name", idx));
        }
        if rule.matches.is_empty() {
            return Err(format!(
                "rule '{}' has empty matches block — would match every action",
                rule.name
            ));
        }
        validate_match_block(&rule.matches, &rule.name)?;
    }
    Ok(())
}

fn validate_match_block(m: &Match, rule_name: &str) -> Result<(), String> {
    if let Some(sm) = &m.operation {
        validate_string_match(sm, rule_name, "operation")?;
    }
    if let Some(sm) = &m.target {
        validate_string_match(sm, rule_name, "target")?;
    }
    for am in m.arg.iter() {
        validate_string_match(&am.key, rule_name, "arg.key")?;
        if let Some(sm) = &am.value {
            validate_string_match(sm, rule_name, "arg.value")?;
        }
    }
    Ok(())
}

fn validate_string_match(sm: &StringMatch, rule_name: &str, field: &str) -> Result<(), String> {
    let bytes = match sm {
        StringMatch::Exact(s) => s.as_bytes(),
        StringMatch::OneOf { one_of } => {
            for s in one_of {
                if s.as_bytes().contains(&0) {
                    return Err(format!(
                        "rule '{}' {} contains NUL byte in one_of[]",
                        rule_name, field
                    ));
                }
            }
            return Ok(());
        }
        StringMatch::Glob { glob } => glob.as_bytes(),
    };
    if bytes.contains(&0) {
        return Err(format!("rule '{}' {} contains NUL byte", rule_name, field));
    }
    Ok(())
}

fn candidate_policy_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("COSH_AUDIT_POLICY") {
        if !p.is_empty() {
            out.push(PathBuf::from(p));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(user_policy_path_for_home(Path::new(&home)));
    }
    out.push(PathBuf::from("/etc/cosh/audit.toml"));
    out
}

fn user_policy_path_for_home(home: &Path) -> PathBuf {
    home.join(".copilot-shell/cosh/audit.toml")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosh_types::audit::Outcome;

    #[test]
    fn loads_minimal_policy() {
        let toml_src = r#"
            version = "v1"
            default = "Deny"
            [[rules]]
            name = "x"
            matches.subsystem = "pkg"
            outcome = "Allow"
        "#;
        let p = parse_and_validate(toml_src.as_bytes()).unwrap();
        assert_eq!(p.version, "v1");
        assert_eq!(p.default, Outcome::Deny);
        assert_eq!(p.rules.len(), 1);
    }

    #[test]
    fn rejects_unknown_field_in_policy() {
        let toml_src = r#"
            version = "v1"
            default = "Deny"
            unexpected = 1
        "#;
        let err = parse_and_validate(toml_src.as_bytes()).unwrap_err();
        assert!(err.contains("unknown field") || err.contains("unexpected"));
    }

    #[test]
    fn rejects_unknown_field_in_rule() {
        let toml_src = r#"
            version = "v1"
            default = "Deny"
            [[rules]]
            name = "x"
            matches.subsystem = "pkg"
            outcome = "Allow"
            extra = "nope"
        "#;
        let err = parse_and_validate(toml_src.as_bytes()).unwrap_err();
        assert!(err.contains("unknown field") || err.contains("extra"));
    }

    #[test]
    fn rejects_empty_match_block() {
        let toml_src = r#"
            version = "v1"
            default = "Deny"
            [[rules]]
            name = "everything"
            matches = {}
            outcome = "Allow"
        "#;
        let err = parse_and_validate(toml_src.as_bytes()).unwrap_err();
        assert!(err.contains("empty matches block"), "got: {}", err);
    }

    #[test]
    fn rejects_invalid_outcome() {
        let toml_src = r#"
            version = "v1"
            default = "Yolo"
        "#;
        let err = parse_and_validate(toml_src.as_bytes()).unwrap_err();
        assert!(err.contains("toml parse error"), "got: {}", err);
    }

    #[test]
    fn policy_version_includes_hash() {
        // load builtin balanced via from_builtin and check shape
        let bal = builtin::balanced();
        assert!(bal.policy_version.starts_with("builtin-balanced@"));
        assert!(bal.policy_version.contains("+sha256:"));
        // hash portion must be hex of expected length
        let after = bal.policy_version.split("+sha256:").nth(1).unwrap();
        assert_eq!(after.len(), 64);
        assert!(after.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn from_user_file_loads_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.toml");
        std::fs::write(
            &path,
            r#"
                version = "v1-test"
                default = "Allow"
                [[rules]]
                name = "deny-rm"
                matches.subsystem = "shell"
                matches.operation = "rm"
                outcome = "Deny"
            "#,
        )
        .unwrap();
        let loaded = LoadedPolicy::from_user_file(&path).unwrap();
        assert_eq!(loaded.policy.version, "v1-test");
        assert!(loaded.policy_version.starts_with("user@v1-test+sha256:"));
    }

    #[test]
    fn user_policy_path_uses_copilot_shell_cosh_dir() {
        assert_eq!(
            user_policy_path_for_home(Path::new("/tmp/cosh-home")),
            PathBuf::from("/tmp/cosh-home/.copilot-shell/cosh/audit.toml")
        );
    }
}
