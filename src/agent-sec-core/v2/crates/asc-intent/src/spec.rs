//! Deterministic spec-to-Cedar generation; no model, no file reads, no enforcement.
//!
//! An [`IntentSpec`] is a closed allow/deny rule set over the shared schema. Rendering is a pure
//! function of the spec, and [`compile_spec`] runs the same Strict validation as the model path,
//! so a generated artifact is never trusted more than a model draft.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::policy_validation::{MAX_POLICIES, absolute_path};
use crate::{Artifact, PolicyError, prepare_policy};

/// Version of the deterministic spec format.
pub const SPEC_VERSION: &str = "asc.intent.spec.v1";

/// Explicit allow/deny rules; every path is a canonical absolute literal or directory subtree.
///
/// Deny rules render as `forbid`, which takes precedence over `permit`; with no matching permit
/// Cedar denies by default, so an allow-only spec already blocks everything it does not name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IntentSpec {
    /// Spec format version; must equal [`SPEC_VERSION`].
    pub format_version: String,
    /// Paths granted `file.read`; a trailing `/*` or `/**` grants the whole subtree.
    pub allow_read: Vec<String>,
    /// Paths granted `file.write`.
    pub allow_write: Vec<String>,
    /// Executable paths granted `file.exec`.
    pub allow_exec: Vec<String>,
    /// Paths denied `file.read`, carving exceptions out of broader allows.
    pub deny_read: Vec<String>,
    /// Paths denied `file.write`.
    pub deny_write: Vec<String>,
    /// Executable paths denied `file.exec`.
    pub deny_exec: Vec<String>,
    /// Grants `network.use` for the whole network; the schema has no endpoint granularity.
    pub allow_network: bool,
    /// Denies `network.use` for the whole network.
    pub deny_network: bool,
    /// Restricts read/write rules to these requesting programs; exact paths only. `file.exec`
    /// rules are unaffected: the schema records no requesting program for an invocation, whose
    /// target executable is already the rule's resource path.
    pub programs: Vec<String>,
}

impl Default for IntentSpec {
    fn default() -> Self {
        Self {
            format_version: SPEC_VERSION.into(),
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_exec: Vec::new(),
            deny_read: Vec::new(),
            deny_write: Vec::new(),
            deny_exec: Vec::new(),
            allow_network: false,
            deny_network: false,
            programs: Vec::new(),
        }
    }
}

/// Spec failures name the offending input: specs are explicit local flags, not model or task
/// content, so echoing them back is the fastest way to a fix.
#[derive(Debug, Error)]
pub enum SpecError {
    /// The spec format version is not [`SPEC_VERSION`].
    #[error("unsupported spec format version; expected {SPEC_VERSION}")]
    InvalidVersion,
    /// A path is relative, aliased, oversized or carries characters the template cannot express.
    #[error(
        "path must be a canonical absolute path without '*', backslashes or control characters: {0}"
    )]
    InvalidPath(String),
    /// A program constraint is not an exact canonical absolute path.
    #[error("program must be an exact canonical absolute path without subtree suffix: {0}")]
    InvalidProgram(String),
    /// No rule was selected; emitting an empty policy would silently deny every action.
    #[error("spec defines no rules; Cedar default-deny would block every action")]
    EmptySpec,
    /// Program constraints only apply to file rules, so a network-only spec cannot carry them.
    #[error("programs require at least one file rule")]
    ProgramWithoutFileRule,
    /// The two network switches contradict each other.
    #[error("allow_network and deny_network cannot both be set")]
    ConflictingNetwork,
    /// The rule count exceeds the shared policy budget.
    #[error("spec exceeds the rule budget")]
    TooManyRules,
    /// The rendered template failed Strict validation; this indicates a template defect.
    #[error("generated policy failed validation: {0}")]
    Policy(#[from] PolicyError),
}

impl IntentSpec {
    /// Renders the spec as Cedar text, rejecting invalid or contradictory input.
    ///
    /// Output is deterministic: rules follow a fixed action order, duplicates collapse, and
    /// identical specs render byte-for-byte identically.
    ///
    /// # Errors
    /// Rejects unknown versions, invalid paths, empty or contradictory rule sets and oversized
    /// specs.
    pub fn render_cedar(&self) -> Result<String, SpecError> {
        if self.format_version != SPEC_VERSION {
            return Err(SpecError::InvalidVersion);
        }
        if self.allow_network && self.deny_network {
            return Err(SpecError::ConflictingNetwork);
        }
        let file_lists = [
            &self.allow_read,
            &self.deny_read,
            &self.allow_write,
            &self.deny_write,
            &self.allow_exec,
            &self.deny_exec,
        ];
        let rule_count = file_lists.iter().map(|list| list.len()).sum::<usize>()
            + usize::from(self.allow_network)
            + usize::from(self.deny_network);
        if rule_count == 0 {
            return Err(SpecError::EmptySpec);
        }
        if rule_count > MAX_POLICIES {
            return Err(SpecError::TooManyRules);
        }
        if !self.programs.is_empty() && !file_lists.iter().any(|list| !list.is_empty()) {
            return Err(SpecError::ProgramWithoutFileRule);
        }
        let mut programs = Vec::new();
        for raw in &self.programs {
            let program = parse_program(raw)?;
            if !programs.contains(&program) {
                programs.push(program);
            }
        }
        let mut out = String::new();
        for (effect, action, paths) in [
            ("permit", "file.read", &self.allow_read),
            ("forbid", "file.read", &self.deny_read),
            ("permit", "file.write", &self.allow_write),
            ("forbid", "file.write", &self.deny_write),
            ("permit", "file.exec", &self.allow_exec),
            ("forbid", "file.exec", &self.deny_exec),
        ] {
            let mut seen = HashSet::new();
            for raw in paths {
                let pattern = parse_path(raw)?;
                if seen.insert(pattern.clone()) {
                    // FileExecContext carries no requesting program, so the constraint only
                    // applies where the schema actually records one.
                    let rule_programs: &[String] = if action == "file.exec" {
                        &[]
                    } else {
                        &programs
                    };
                    push_file_rule(&mut out, effect, action, &pattern, rule_programs);
                }
            }
        }
        if self.allow_network {
            push_network_rule(&mut out, "permit");
        }
        if self.deny_network {
            push_network_rule(&mut out, "forbid");
        }
        Ok(out)
    }
}

/// Renders and Strict-validates a spec through the same gate as model-generated Cedar.
///
/// # Errors
/// Propagates every [`SpecError`] and, if a template ever regresses, the underlying policy
/// validation failure.
pub fn compile_spec(spec: &IntentSpec) -> Result<Artifact, SpecError> {
    let text = spec.render_cedar()?;
    Ok(prepare_policy(&text)?)
}

/// A path literal or a directory subtree; the subtree prefix never carries a trailing slash
/// except for the root itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PathPattern {
    Exact(String),
    Subtree(String),
}

fn parse_path(raw: &str) -> Result<PathPattern, SpecError> {
    // Cedar `like` treats every '*' as a wildcard; refusing the character keeps the rendered
    // pattern exactly as broad as the caller asked, with no escaping subtleties.
    if raw.contains('*') && !raw.ends_with("/*") && !raw.ends_with("/**") {
        return Err(SpecError::InvalidPath(raw.to_owned()));
    }
    if let Some(prefix) = raw.strip_suffix("/**").or_else(|| raw.strip_suffix("/*")) {
        let prefix = if prefix.is_empty() { "/" } else { prefix };
        if !prefix.contains('*') && absolute_path(prefix) {
            return Ok(PathPattern::Subtree(prefix.to_owned()));
        }
        return Err(SpecError::InvalidPath(raw.to_owned()));
    }
    if absolute_path(raw) {
        Ok(PathPattern::Exact(raw.to_owned()))
    } else {
        Err(SpecError::InvalidPath(raw.to_owned()))
    }
}

fn parse_program(raw: &str) -> Result<String, SpecError> {
    match parse_path(raw) {
        Ok(PathPattern::Exact(path)) => Ok(path),
        _ => Err(SpecError::InvalidProgram(raw.to_owned())),
    }
}

fn push_file_rule(
    out: &mut String,
    effect: &str,
    action: &str,
    pattern: &PathPattern,
    programs: &[String],
) {
    let path_condition = match pattern {
        PathPattern::Exact(path) => format!("resource.path == {}", cedar_string(path)),
        PathPattern::Subtree(prefix) => {
            let glob = if prefix == "/" {
                "/*".to_owned()
            } else {
                format!("{prefix}/*")
            };
            format!("resource.path like {}", cedar_string(&glob))
        }
    };
    let condition = if programs.is_empty() {
        path_condition
    } else {
        let alternatives = programs
            .iter()
            .map(|program| format!("context.input.program == {}", cedar_string(program)))
            .collect::<Vec<_>>()
            .join(" || ");
        format!("{path_condition} && ({alternatives})")
    };
    // String's fmt::Write implementation cannot return an error.
    let _ = write!(
        out,
        "{effect} (\n  principal is SecCore::Subject,\n  action == SecCore::Action::{},\n  resource is SecCore::File\n)\nwhen {{ {condition} }};\n",
        cedar_string(action)
    );
}

fn push_network_rule(out: &mut String, effect: &str) {
    // String's fmt::Write implementation cannot return an error.
    let _ = write!(
        out,
        "{effect} (\n  principal is SecCore::Subject,\n  action == SecCore::Action::\"network.use\",\n  resource == SecCore::Network::\"all\"\n);\n"
    );
}

fn cedar_string(value: &str) -> String {
    // Path validation already rejects backslashes and control characters, so quotes are the only
    // character that still needs escaping inside a Cedar string literal.
    format!("\"{}\"", value.replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with_read(path: &str) -> IntentSpec {
        IntentSpec {
            allow_read: vec![path.into()],
            ..IntentSpec::default()
        }
    }

    #[test]
    fn every_rule_kind_renders_and_passes_strict_validation() {
        let spec = IntentSpec {
            allow_read: vec!["/data/**".into(), "/etc/hosts".into()],
            allow_write: vec!["/data/out/*".into()],
            allow_exec: vec!["/usr/bin/python3".into()],
            deny_read: vec!["/data/secrets/**".into()],
            deny_write: vec!["/data/out/locked.txt".into()],
            deny_exec: vec!["/usr/bin/rm".into()],
            allow_network: false,
            deny_network: true,
            programs: vec!["/usr/bin/python3".into(), "/usr/bin/bash".into()],
            ..IntentSpec::default()
        };
        let text = spec.render_cedar().unwrap();
        assert!(text.contains("resource.path like \"/data/*\""));
        assert!(text.contains("resource.path == \"/etc/hosts\""));
        assert!(text.contains("forbid"));
        assert!(text.contains("SecCore::Action::\"network.use\""));
        assert!(text.contains(
            "(context.input.program == \"/usr/bin/python3\" || context.input.program == \"/usr/bin/bash\")"
        ));
        // Five read/write rules carry two program comparisons each; the two exec rules carry none.
        assert_eq!(text.matches("context.input.program").count(), 10);
        let artifact = compile_spec(&spec).unwrap();
        assert_eq!(artifact.language, "cedar");
        assert_eq!(artifact.text, text);
    }

    #[test]
    fn network_only_specs_render_both_switches() {
        for (allow, effect) in [(true, "permit"), (false, "forbid")] {
            let spec = IntentSpec {
                allow_network: allow,
                deny_network: !allow,
                ..IntentSpec::default()
            };
            let artifact = compile_spec(&spec).unwrap();
            assert!(artifact.text.starts_with(effect));
            assert!(artifact.text.contains("SecCore::Network::\"all\""));
        }
    }

    #[test]
    fn subtree_forms_are_equivalent_and_root_subtree_matches_everything() {
        let star = compile_spec(&spec_with_read("/data/*")).unwrap();
        let double = compile_spec(&spec_with_read("/data/**")).unwrap();
        assert_eq!(star.text, double.text);
        let root = compile_spec(&spec_with_read("/**")).unwrap();
        assert!(root.text.contains("resource.path like \"/*\""));
    }

    #[test]
    fn rendering_is_deterministic_and_collapses_duplicates() {
        let spec = IntentSpec {
            allow_read: vec!["/data/**".into(), "/data/*".into(), "/data/**".into()],
            programs: vec!["/usr/bin/python3".into(), "/usr/bin/python3".into()],
            ..IntentSpec::default()
        };
        let first = spec.render_cedar().unwrap();
        assert_eq!(first, spec.render_cedar().unwrap());
        assert_eq!(first.matches("permit").count(), 1);
        assert_eq!(first.matches("context.input.program").count(), 1);
    }

    #[test]
    fn quotes_in_paths_are_escaped_and_still_validate() {
        let spec = spec_with_read("/data/we\"ird/file.txt");
        let artifact = compile_spec(&spec).unwrap();
        assert!(
            artifact
                .text
                .contains("resource.path == \"/data/we\\\"ird/file.txt\"")
        );
    }

    #[test]
    fn invalid_paths_programs_and_versions_are_rejected() {
        for path in [
            "relative/file",
            "/data/../etc",
            "/data//x",
            "/data/./x",
            "/data\\x",
            "/data/\u{1b}x",
            "/data/we*ird",
            "/data/x*",
            "",
        ] {
            assert!(matches!(
                spec_with_read(path).render_cedar(),
                Err(SpecError::InvalidPath(_))
            ));
        }
        for program in ["/usr/bin/python3/**", "relative"] {
            let spec = IntentSpec {
                programs: vec![program.into()],
                ..spec_with_read("/data/**")
            };
            assert!(matches!(
                spec.render_cedar(),
                Err(SpecError::InvalidProgram(_))
            ));
        }
        let mut versioned = spec_with_read("/data/**");
        versioned.format_version = "asc.intent.spec.v999".into();
        assert!(matches!(
            versioned.render_cedar(),
            Err(SpecError::InvalidVersion)
        ));
    }

    #[test]
    fn empty_contradictory_and_mispaired_specs_are_rejected() {
        assert!(matches!(
            IntentSpec::default().render_cedar(),
            Err(SpecError::EmptySpec)
        ));
        let both = IntentSpec {
            allow_network: true,
            deny_network: true,
            ..IntentSpec::default()
        };
        assert!(matches!(
            both.render_cedar(),
            Err(SpecError::ConflictingNetwork)
        ));
        let network_program = IntentSpec {
            allow_network: true,
            programs: vec!["/usr/bin/python3".into()],
            ..IntentSpec::default()
        };
        assert!(matches!(
            network_program.render_cedar(),
            Err(SpecError::ProgramWithoutFileRule)
        ));
        let oversized = IntentSpec {
            allow_read: (0..=MAX_POLICIES).map(|i| format!("/f{i}")).collect(),
            ..IntentSpec::default()
        };
        assert!(matches!(
            oversized.render_cedar(),
            Err(SpecError::TooManyRules)
        ));
    }

    #[test]
    fn spec_json_round_trips_and_rejects_unknown_fields() {
        let spec = spec_with_read("/data/**");
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: IntentSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);
        let mut unknown = serde_json::to_value(&spec).unwrap();
        unknown["trusted"] = serde_json::json!(true);
        assert!(serde_json::from_value::<IntentSpec>(unknown).is_err());
        let minimal: IntentSpec = serde_json::from_str("{\"allow_read\":[\"/data\"]}").unwrap();
        assert_eq!(minimal.format_version, SPEC_VERSION);
    }
}
