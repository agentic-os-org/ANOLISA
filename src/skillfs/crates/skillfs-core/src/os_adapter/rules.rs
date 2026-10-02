//! Rule-artifact schema, direction/eligibility parsing, validation, and
//! compilation into an ordered substitution table with per-rule match modes.
//!
//! Eligibility is governed **only** by the explicit per-rule `auto_apply`
//! field. `confidence` and `notes` are accepted as human annotations but carry
//! no behavior — SkillFS does not inherit any confidence-driven semantics.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;

use super::detect::OsTarget;
use super::error::OsAdapterError;

/// Rule as it appears in the YAML artifact. Unknown fields are rejected so
/// typos surface as load errors instead of being silently ignored.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    ubuntu: String,
    alinux: String,
    direction: String,
    /// Optional matching semantics. Missing preserves the historical literal
    /// substring behavior for existing built-in and external artifacts.
    #[serde(default, rename = "match")]
    match_mode: Option<String>,
    /// Explicit eligibility. Optional at the serde layer so a legacy artifact
    /// that omits it fails with a dedicated, indexed [`OsAdapterError`] rather
    /// than an opaque serde "missing field" error.
    #[serde(default)]
    auto_apply: Option<String>,
    /// Accepted human annotation; carries no behavior.
    #[serde(default)]
    #[allow(dead_code)]
    confidence: Option<String>,
    /// Accepted human annotation; carries no behavior.
    #[serde(default)]
    #[allow(dead_code)]
    notes: Option<String>,
}

/// Matching semantics attached to one compiled source pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MatchMode {
    /// Match the source at any scanned substring position.
    Literal,
    /// Require ASCII-alphanumeric boundaries at alphanumeric source edges.
    Token,
}

impl MatchMode {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value.unwrap_or("literal") {
            "literal" => Some(Self::Literal),
            "token" => Some(Self::Token),
            _ => None,
        }
    }

    /// Whether `source` matches `input` at `start` under this mode.
    pub(crate) fn matches(self, input: &str, start: usize, source: &str) -> bool {
        let rest = &input[start..];
        if !rest.starts_with(source) {
            return false;
        }
        if self == Self::Literal {
            return true;
        }

        let source_bytes = source.as_bytes();
        let Some(first) = source_bytes.first() else {
            return false;
        };
        let Some(last) = source_bytes.last() else {
            return false;
        };
        let input_bytes = input.as_bytes();
        let left_is_bounded = !first.is_ascii_alphanumeric()
            || start == 0
            || !input_bytes[start - 1].is_ascii_alphanumeric();
        let end = start + source.len();
        let right_is_bounded = !last.is_ascii_alphanumeric()
            || input_bytes
                .get(end)
                .is_none_or(|byte| !byte.is_ascii_alphanumeric());
        left_is_bounded && right_is_bounded
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Bidirectional,
    UbuntuToAlinuxOnly,
    AlinuxToUbuntuOnly,
}

impl Direction {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "bidirectional" => Some(Direction::Bidirectional),
            "ubuntu_to_alinux_only" => Some(Direction::UbuntuToAlinuxOnly),
            "alinux_to_ubuntu_only" => Some(Direction::AlinuxToUbuntuOnly),
            _ => None,
        }
    }

    /// Whether this rule contributes a substitution when converting **to**
    /// `target`. The source side is the opposite OS of `target`.
    fn applies_to(self, target: OsTarget) -> bool {
        matches!(
            (self, target),
            (Direction::Bidirectional, _)
                | (Direction::UbuntuToAlinuxOnly, OsTarget::Alinux)
                | (Direction::AlinuxToUbuntuOnly, OsTarget::Ubuntu)
        )
    }
}

/// Parsed, validated, and direction-filtered substitution table for one target.
#[derive(Debug)]
pub(crate) struct CompiledRules {
    /// Substitutions in file order; applied by a single-pass, longest-match
    /// scan (file order only breaks length ties, which distinct sources cannot
    /// produce).
    pub rules: Vec<CompiledRule>,
    /// Source literals that must be matched-and-preserved during the scan, so a
    /// shorter eligible rule cannot rewrite inside them. These come from rules
    /// that are ineligible for this target — `auto_apply: never`, identity
    /// (`from == to`), or direction-disallowed. A source/match-mode pair that is
    /// also an eligible substitution is excluded (substitution wins), so
    /// protection never suppresses a real mapping. Different modes for the same
    /// source coexist and are evaluated independently by the scanner.
    pub protects: Vec<CompiledProtection>,
    /// Number of rules parsed from the artifact (before direction filtering).
    pub total_rules: usize,
}

/// One eligible compiled substitution.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CompiledRule {
    /// Source-side pattern for the resolved direction.
    pub source: String,
    /// Replacement emitted when the pattern matches.
    pub target: String,
    /// Boundary behavior for this source pattern.
    pub match_mode: MatchMode,
}

/// One ineligible source retained as a protection match.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CompiledProtection {
    /// Source-side pattern retained verbatim when it matches.
    pub source: String,
    /// Boundary behavior for this protection pattern.
    pub match_mode: MatchMode,
}

/// Parse `bytes`, validate every rule, and compile the substitutions eligible
/// for `target`.
///
/// # Errors
///
/// Returns [`OsAdapterError`] for malformed YAML, an empty rule list, an
/// invalid `direction`/`auto_apply`/`match`, an empty pattern, or
/// duplicate/ambiguous source patterns for the resolved target.
pub(crate) fn compile(
    bytes: &[u8],
    target: OsTarget,
    path: &Path,
) -> Result<CompiledRules, OsAdapterError> {
    let raw_rules: Vec<RawRule> =
        serde_yaml::from_slice(bytes).map_err(|source| OsAdapterError::Yaml {
            path: path.to_path_buf(),
            source,
        })?;
    if raw_rules.is_empty() {
        return Err(OsAdapterError::EmptyRules {
            path: path.to_path_buf(),
        });
    }
    let total_rules = raw_rules.len();

    let mut rules: Vec<CompiledRule> = Vec::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    // Sources of ineligible rules (never / identity / direction-disallowed).
    // Kept so their full span is preserved during the longest-match scan and a
    // shorter eligible rule cannot rewrite inside them. Substitutions win, so we
    // strip any source/match-mode pair that is also eligible afterward.
    let mut protects: Vec<CompiledProtection> = Vec::new();
    let mut protect_seen: HashSet<(String, MatchMode)> = HashSet::new();
    for (index, raw) in raw_rules.iter().enumerate() {
        let direction =
            Direction::parse(&raw.direction).ok_or_else(|| OsAdapterError::InvalidDirection {
                index,
                value: raw.direction.clone(),
            })?;
        let auto_apply = match raw.auto_apply.as_deref() {
            Some("always") => true,
            Some("never") => false,
            Some(other) => {
                return Err(OsAdapterError::InvalidAutoApply {
                    index,
                    value: other.to_string(),
                });
            }
            None => return Err(OsAdapterError::MissingAutoApply { index }),
        };
        let match_mode = MatchMode::parse(raw.match_mode.as_deref()).ok_or_else(|| {
            OsAdapterError::InvalidMatchMode {
                index,
                value: raw.match_mode.clone().unwrap_or_default(),
            }
        })?;
        if raw.ubuntu.is_empty() {
            return Err(OsAdapterError::EmptyPattern {
                index,
                field: "ubuntu",
            });
        }
        if raw.alinux.is_empty() {
            return Err(OsAdapterError::EmptyPattern {
                index,
                field: "alinux",
            });
        }

        // Source is the opposite side of the target we convert toward.
        let (from, to) = match target {
            OsTarget::Alinux => (&raw.ubuntu, &raw.alinux),
            OsTarget::Ubuntu => (&raw.alinux, &raw.ubuntu),
        };

        // A rule contributes a real substitution only when it is eligible for
        // this target and actually changes bytes. Everything else (never,
        // direction-disallowed, or identity `from == to`) becomes a protection
        // source so eligibility is not bypassed by a shorter overlapping rule.
        let is_substitution = auto_apply && direction.applies_to(target) && from != to;
        if !is_substitution {
            if protect_seen.insert((from.clone(), match_mode)) {
                protects.push(CompiledProtection {
                    source: from.clone(),
                    match_mode,
                });
            }
            continue;
        }

        if let Some(existing) = seen.get(from) {
            if existing == to {
                return Err(OsAdapterError::DuplicateRule {
                    pattern: from.clone(),
                    target: target.as_str(),
                });
            }
            return Err(OsAdapterError::AmbiguousRule {
                pattern: from.clone(),
                target: target.as_str(),
                existing: existing.clone(),
                conflicting: to.clone(),
            });
        }
        seen.insert(from.clone(), to.clone());
        rules.push(CompiledRule {
            source: from.clone(),
            target: to.clone(),
            match_mode,
        });
    }

    // Substitutions take precedence over protection for the same source/mode
    // pair, so the canonical reverse mapping (e.g. `dnf install -y ` ->
    // `apt-get install -y`) is never suppressed by a direction-disallowed
    // alternate that shares it.
    let active_patterns: HashSet<(&str, MatchMode)> = rules
        .iter()
        .map(|rule| (rule.source.as_str(), rule.match_mode))
        .collect();
    protects.retain(|protection| {
        !active_patterns.contains(&(protection.source.as_str(), protection.match_mode))
    });

    Ok(CompiledRules {
        rules,
        protects,
        total_rules,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn compile_str(yaml: &str, target: OsTarget) -> Result<CompiledRules, OsAdapterError> {
        compile(yaml.as_bytes(), target, Path::new("test-rules.yaml"))
    }

    const SAMPLE: &str = r#"
- ubuntu: "sudo apt-get install -y "
  alinux: "sudo dnf install -y "
  direction: bidirectional
  auto_apply: always
  confidence: high
- ubuntu: "libssl-dev"
  alinux: "openssl-devel"
  direction: bidirectional
  auto_apply: always
- ubuntu: "apache2.service"
  alinux: "httpd.service"
  direction: bidirectional
  auto_apply: always
- ubuntu: "build-essential"
  alinux: '@"Development Tools"'
  direction: ubuntu_to_alinux_only
  auto_apply: always
- ubuntu: "ufw"
  alinux: "firewalld"
  direction: ubuntu_to_alinux_only
  auto_apply: never
"#;

    #[test]
    fn ubuntu_to_alinux_registers_eligible_rules() {
        let c = compile_str(SAMPLE, OsTarget::Alinux).unwrap();
        // 3 bidirectional + 1 forward-only always; the "never" rule is skipped.
        assert_eq!(c.rules.len(), 4);
        assert_eq!(c.total_rules, 5);
    }

    #[test]
    fn alinux_to_ubuntu_only_reverses_bidirectional() {
        let c = compile_str(SAMPLE, OsTarget::Ubuntu).unwrap();
        // Only the 3 bidirectional rules reverse; forward-only rules do not.
        assert_eq!(c.rules.len(), 3);
    }

    #[test]
    fn invalid_direction_is_rejected() {
        let yaml = "- ubuntu: a\n  alinux: b\n  direction: sideways\n  auto_apply: always\n";
        assert!(matches!(
            compile_str(yaml, OsTarget::Alinux).unwrap_err(),
            OsAdapterError::InvalidDirection { .. }
        ));
    }

    #[test]
    fn invalid_auto_apply_is_rejected() {
        let yaml = "- ubuntu: a\n  alinux: b\n  direction: bidirectional\n  auto_apply: maybe\n";
        assert!(matches!(
            compile_str(yaml, OsTarget::Alinux).unwrap_err(),
            OsAdapterError::InvalidAutoApply { .. }
        ));
    }

    #[test]
    fn invalid_match_mode_is_rejected() {
        let yaml = "- ubuntu: a\n  alinux: b\n  direction: bidirectional\n  match: prefix\n  auto_apply: always\n";
        assert!(matches!(
            compile_str(yaml, OsTarget::Alinux).unwrap_err(),
            OsAdapterError::InvalidMatchMode { .. }
        ));
    }

    #[test]
    fn match_mode_defaults_to_literal_and_accepts_token() {
        let yaml = "- ubuntu: apt\n  alinux: dnf\n  direction: bidirectional\n  auto_apply: always\n- ubuntu: cron\n  alinux: cronie\n  direction: bidirectional\n  match: token\n  auto_apply: always\n";
        let compiled = compile_str(yaml, OsTarget::Alinux).unwrap();
        assert_eq!(compiled.rules[0].match_mode, MatchMode::Literal);
        assert_eq!(compiled.rules[1].match_mode, MatchMode::Token);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let yaml = "- ubuntu: a\n  alinux: b\n  direction: bidirectional\n  auto_apply: always\n  bogus: 1\n";
        assert!(matches!(
            compile_str(yaml, OsTarget::Alinux).unwrap_err(),
            OsAdapterError::Yaml { .. }
        ));
    }

    #[test]
    fn malformed_yaml_is_rejected() {
        assert!(matches!(
            compile_str("this: [is: not: valid", OsTarget::Alinux).unwrap_err(),
            OsAdapterError::Yaml { .. }
        ));
    }

    #[test]
    fn empty_rules_are_rejected() {
        assert!(matches!(
            compile_str("[]\n", OsTarget::Alinux).unwrap_err(),
            OsAdapterError::EmptyRules { .. }
        ));
    }

    #[test]
    fn empty_pattern_is_rejected() {
        let yaml =
            "- ubuntu: \"\"\n  alinux: b\n  direction: bidirectional\n  auto_apply: always\n";
        assert!(matches!(
            compile_str(yaml, OsTarget::Alinux).unwrap_err(),
            OsAdapterError::EmptyPattern {
                field: "ubuntu",
                ..
            }
        ));
    }

    #[test]
    fn duplicate_rule_is_rejected() {
        let yaml = "- ubuntu: apt\n  alinux: dnf\n  direction: bidirectional\n  auto_apply: always\n- ubuntu: apt\n  alinux: dnf\n  direction: bidirectional\n  auto_apply: always\n";
        assert!(matches!(
            compile_str(yaml, OsTarget::Alinux).unwrap_err(),
            OsAdapterError::DuplicateRule { .. }
        ));
    }

    #[test]
    fn ambiguous_rule_is_rejected() {
        let yaml = "- ubuntu: apt\n  alinux: dnf\n  direction: bidirectional\n  auto_apply: always\n- ubuntu: apt\n  alinux: yum\n  direction: bidirectional\n  auto_apply: always\n";
        assert!(matches!(
            compile_str(yaml, OsTarget::Alinux).unwrap_err(),
            OsAdapterError::AmbiguousRule { .. }
        ));
    }

    #[test]
    fn identity_rule_does_not_trip_conflict_detection() {
        let yaml = "- ubuntu: nginx\n  alinux: nginx\n  direction: bidirectional\n  auto_apply: always\n- ubuntu: /etc/nginx/\n  alinux: /etc/nginx/\n  direction: bidirectional\n  auto_apply: always\n";
        assert_eq!(compile_str(yaml, OsTarget::Alinux).unwrap().rules.len(), 0);
    }

    // -----------------------------------------------------------------------
    // Provider rule contract
    // -----------------------------------------------------------------------

    #[test]
    fn legacy_rule_without_auto_apply_is_rejected_with_index() {
        // A legacy artifact (confidence but no auto_apply) must fail with the
        // dedicated MissingAutoApply error naming the offending rule index.
        let yaml = "- ubuntu: apt-get\n  alinux: dnf\n  direction: bidirectional\n- ubuntu: g++\n  alinux: gcc-c++\n  direction: bidirectional\n  confidence: high\n";
        let err = compile_str(yaml, OsTarget::Alinux).unwrap_err();
        assert!(matches!(err, OsAdapterError::MissingAutoApply { index: 0 }));
        assert!(format!("{err}").contains("auto_apply"));
    }

    #[test]
    fn confidence_and_notes_are_inert_annotations() {
        // confidence/notes are accepted but never drive eligibility: a
        // confidence=low rule with auto_apply=always is still applied, and a
        // confidence=high rule with auto_apply=never is still skipped.
        let yaml = "- ubuntu: apt-get\n  alinux: dnf\n  direction: bidirectional\n  auto_apply: always\n  confidence: low\n  notes: applied despite low confidence\n- ubuntu: ufw\n  alinux: firewalld\n  direction: ubuntu_to_alinux_only\n  auto_apply: never\n  confidence: high\n";
        let c = compile_str(yaml, OsTarget::Alinux).unwrap();
        assert_eq!(c.rules.len(), 1);
        assert_eq!(c.rules[0].source, "apt-get");
        assert_eq!(c.rules[0].target, "dnf");
        assert_eq!(c.rules[0].match_mode, MatchMode::Literal);
    }

    /// Provider-contract fixture: multiple Ubuntu spellings map to one Alinux
    /// package. Reverse ambiguity is resolved *explicitly* by marking exactly
    /// one pair `bidirectional` (the canonical reverse) and the alternates
    /// `ubuntu_to_alinux_only`. This is how a provider artifact must express a
    /// many-to-one forward mapping without a reverse conflict.
    const CANONICAL_REVERSE_FIXTURE: &str = r#"
- ubuntu: "libncurses-dev"
  alinux: "ncurses-devel"
  direction: bidirectional
  auto_apply: always
- ubuntu: "libncurses5-dev"
  alinux: "ncurses-devel"
  direction: ubuntu_to_alinux_only
  auto_apply: always
- ubuntu: "libncursesw5-dev"
  alinux: "ncurses-devel"
  direction: ubuntu_to_alinux_only
  auto_apply: always
"#;

    #[test]
    fn canonical_fixture_loads_forward_without_conflict() {
        // Forward (target Alinux): three distinct source spellings -> one
        // target. Distinct sources, so no conflict.
        let c = compile_str(CANONICAL_REVERSE_FIXTURE, OsTarget::Alinux).unwrap();
        assert_eq!(c.rules.len(), 3);
    }

    #[test]
    fn canonical_fixture_loads_reverse_unambiguously() {
        // Reverse (target Ubuntu): only the single bidirectional rule reverses,
        // so `ncurses-devel` maps to exactly one Ubuntu spelling.
        let c = compile_str(CANONICAL_REVERSE_FIXTURE, OsTarget::Ubuntu).unwrap();
        assert_eq!(c.rules.len(), 1);
        assert_eq!(c.rules[0].source, "ncurses-devel");
        assert_eq!(c.rules[0].target, "libncurses-dev");
    }

    #[test]
    fn unresolved_reverse_ambiguity_is_rejected() {
        // The wrong way to express many-to-one: two bidirectional rules pointing
        // different sources at the same target collide on the reverse target.
        let yaml = r#"
- ubuntu: "libncurses-dev"
  alinux: "ncurses-devel"
  direction: bidirectional
  auto_apply: always
- ubuntu: "libncurses5-dev"
  alinux: "ncurses-devel"
  direction: bidirectional
  auto_apply: always
"#;
        // Forward target has distinct sources: fine.
        assert_eq!(compile_str(yaml, OsTarget::Alinux).unwrap().rules.len(), 2);
        // Reverse target: both reverse to `ncurses-devel` -> ambiguous.
        assert!(matches!(
            compile_str(yaml, OsTarget::Ubuntu).unwrap_err(),
            OsAdapterError::AmbiguousRule { .. }
        ));
    }

    #[test]
    fn forward_only_rules_do_not_conflict_for_reverse_target() {
        // Two forward-only rules sharing a target never register for the reverse
        // target, so they cannot create a false reverse conflict.
        let yaml = r#"
- ubuntu: "libncurses5-dev"
  alinux: "ncurses-devel"
  direction: ubuntu_to_alinux_only
  auto_apply: always
- ubuntu: "libncursesw5-dev"
  alinux: "ncurses-devel"
  direction: ubuntu_to_alinux_only
  auto_apply: always
"#;
        assert_eq!(compile_str(yaml, OsTarget::Ubuntu).unwrap().rules.len(), 0);
        assert_eq!(compile_str(yaml, OsTarget::Alinux).unwrap().rules.len(), 2);
    }
}
