//! Findings and the verdict aggregated from them.
//!
//! These types mirror V1's `Finding` and `Verdict`, including the exact summary
//! wording, because the result summary is asserted verbatim by end-to-end
//! goldens. Serialization to the wire is added by the scanner slice, against
//! V1's actual JSON, rather than guessed here.

use crate::rules::{Language, Severity};

/// One rule that matched, with every substring that triggered it.
///
/// Field order follows V1's pydantic model so the serialized document lays out
/// `rule_id, severity, desc_zh, desc_en, evidence` in that order, which the
/// end-to-end goldens assert.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// Identifier of the rule that produced this finding.
    pub rule_id: String,
    /// Severity copied from the rule; drives verdict aggregation.
    pub severity: Severity,
    /// Chinese description, carried for display.
    pub desc_zh: String,
    /// English description, carried for display.
    pub desc_en: String,
    /// Every matched substring, in scan order; never empty for a real finding.
    pub evidence: Vec<String>,
}

/// Outcome of a scan, ordered from least to most severe.
///
/// `Error` is produced by the scanner for a failed scan, not by rule matching,
/// and is kept here so the whole verdict vocabulary lives in one place.
/// Serializes to the lowercase name V1 emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// No rule matched.
    Pass,
    /// Only `warn` rules matched.
    Warn,
    /// At least one `deny` rule matched.
    Deny,
    /// The scan itself failed.
    Error,
}

impl Verdict {
    /// Returns the wire name used by V1.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Deny => "deny",
            Self::Error => "error",
        }
    }
}

/// Aggregates findings into the highest-severity verdict.
///
/// Equivalent to V1's max-severity reduction: with only `warn` and `deny`
/// defined, any `deny` yields [`Verdict::Deny`], any other non-empty set yields
/// [`Verdict::Warn`], and an empty set yields [`Verdict::Pass`].
pub fn compute_verdict(findings: &[Finding]) -> Verdict {
    if findings.is_empty() {
        return Verdict::Pass;
    }
    if findings
        .iter()
        .any(|finding| finding.severity == Severity::Deny)
    {
        Verdict::Deny
    } else {
        Verdict::Warn
    }
}

/// Builds the human-readable summary line.
///
/// The wording, punctuation and rule-id join are byte-for-byte those of V1: the
/// summary is part of the product contract, not a convenience string.
pub fn build_summary(findings: &[Finding], language: Language) -> String {
    if findings.is_empty() {
        return format!("No issues found in {} code", language.as_str());
    }
    let rule_ids: Vec<&str> = findings
        .iter()
        .map(|finding| finding.rule_id.as_str())
        .collect();
    format!(
        "Detected {} issue(s) in {} code: {}",
        findings.len(),
        language.as_str(),
        rule_ids.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(rule_id: &str, severity: Severity) -> Finding {
        Finding {
            rule_id: rule_id.to_owned(),
            severity,
            desc_zh: String::new(),
            desc_en: String::new(),
            evidence: vec!["x".to_owned()],
        }
    }

    #[test]
    fn verdict_takes_the_highest_severity() {
        assert_eq!(compute_verdict(&[]), Verdict::Pass);
        assert_eq!(
            compute_verdict(&[finding("a", Severity::Warn)]),
            Verdict::Warn
        );
        assert_eq!(
            compute_verdict(&[finding("a", Severity::Warn), finding("b", Severity::Deny)]),
            Verdict::Deny
        );
    }

    #[test]
    fn summary_matches_v1_wording() {
        assert_eq!(
            build_summary(&[], Language::Bash),
            "No issues found in bash code"
        );
        assert_eq!(
            build_summary(
                &[
                    finding("shell-disk-wipe", Severity::Warn),
                    finding("shell-reverse-shell", Severity::Warn)
                ],
                Language::Python
            ),
            "Detected 2 issue(s) in python code: shell-disk-wipe, shell-reverse-shell"
        );
    }
}
