//! Regex matching that turns rules and code into findings.
//!
//! A direct port of V1's `regex_engine.py`. Two matching modes exist and must
//! stay behaviourally identical to V1: a plain rule reports every match of its
//! pattern, while a rule carrying `target_regexes` matches at the level of
//! command segments and additionally requires a target hit positioned after the
//! main hit within the same segment.

use std::sync::OnceLock;

use fancy_regex::{Regex, RegexBuilder};

use crate::errors::CodeScanError;
use crate::findings::Finding;
use crate::rules::{Language, RuleDefinition};

/// Minimum work budget for a single shipped-rule match.
///
/// Python's V1 `re` engine has no equivalent 1-million-step ceiling. This floor
/// keeps moderately large, benign snippets from becoming false engine errors.
const MIN_BACKTRACK_LIMIT: usize = 8_000_000;
/// Backtracking work allowed per input byte before the absolute safety cap.
const BACKTRACK_STEPS_PER_BYTE: usize = 128;
/// Hard bound for an RPC frame, including a 4 MiB maximum request body.
const MAX_BACKTRACK_LIMIT: usize = 32_000_000;

/// Command separators used to split code into segments: `;`, newline, `|`, `&&`.
///
/// Mirrors V1's `re.compile(r"[;\n|]|&&")`. `|` is a literal inside the class,
/// and `&&` is a separate alternative tried after the single-character class.
fn segment_separator() -> &'static Regex {
    static SEPARATOR: OnceLock<Regex> = OnceLock::new();
    SEPARATOR.get_or_init(|| {
        // The pattern is a fixed, valid literal, so compilation cannot fail.
        Regex::new(r"[;\n|]|&&").expect("segment separator is a valid regex")
    })
}

/// Collapses newlines inside parentheses into spaces.
///
/// Python allows implicit line continuation inside `()`, so a multi-line call
/// like `open(\n '/etc/shadow'\n)` must read as one logical line for
/// segment-level matching to see the call and its arguments together. Depth is
/// updated before the newline test, exactly as V1 does.
fn normalize_python_parens(code: &str) -> String {
    let mut result = String::with_capacity(code.len());
    let mut depth: usize = 0;
    for ch in code.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch == '\n' && depth > 0 {
            result.push(' ');
        } else {
            result.push(ch);
        }
    }
    result
}

/// Splits `code` on command separators, reproducing Python's `re.split`.
///
/// The separator has no capture groups, so separators are dropped and empty
/// segments between adjacent separators are preserved, matching V1.
///
/// # Errors
/// Returns [`CodeScanError::EngineResource`] if the backtracking matcher
/// exhausts its budget, which cannot happen for this fixed literal pattern but
/// is propagated rather than unwrapped.
fn split_segments(code: &str) -> Result<Vec<String>, CodeScanError> {
    let separator = segment_separator();
    let mut segments = Vec::new();
    let mut last = 0;
    for found in separator.find_iter(code) {
        let matched = found.map_err(|_| CodeScanError::EngineResource)?;
        segments.push(code[last..matched.start()].to_owned());
        last = matched.end();
    }
    segments.push(code[last..].to_owned());
    Ok(segments)
}

/// Compiles a rule pattern, attributing failure to the rule.
///
/// The fancy-regex default budget is lower than the V1 engine's effective
/// capacity for ordinary multi-line input. Scale the bound with input size,
/// while retaining a finite cap for untrusted daemon requests.
///
/// # Errors
/// Returns [`CodeScanError::RegexCompile`] naming the rule when the engine
/// rejects the pattern.
fn compile(pattern: &str, rule_id: &str, input_len: usize) -> Result<Regex, CodeScanError> {
    let mut builder = RegexBuilder::new(pattern);
    builder.backtrack_limit(backtrack_limit(input_len));
    builder
        .build()
        .map_err(|_| CodeScanError::RegexCompile(rule_id.to_owned()))
}

fn backtrack_limit(input_len: usize) -> usize {
    input_len
        .saturating_mul(BACKTRACK_STEPS_PER_BYTE)
        .clamp(MIN_BACKTRACK_LIMIT, MAX_BACKTRACK_LIMIT)
}

/// Segment-level matching for a rule that declares `target_regexes`.
///
/// Splits the code into segments, then keeps a segment when the main pattern
/// matches it and at least one target pattern also matches at a position
/// strictly after the main match. The stripped segment is the evidence, exactly
/// as V1 records it.
///
/// # Errors
/// Returns [`CodeScanError::RegexCompile`] for an unusable pattern, or
/// [`CodeScanError::EngineResource`] if a match exhausts backtracking.
fn match_with_targets(
    code: &str,
    rule: &RuleDefinition,
    targets: &[String],
    language: Language,
) -> Result<Vec<String>, CodeScanError> {
    let normalized;
    let code = if language == Language::Python {
        normalized = normalize_python_parens(code);
        normalized.as_str()
    } else {
        code
    };
    let segments = split_segments(code)?;
    let main = compile(&rule.regex, &rule.rule_id, code.len())?;
    let target_patterns = targets
        .iter()
        .map(|target| compile(target, &rule.rule_id, code.len()))
        .collect::<Result<Vec<_>, _>>()?;

    let mut evidence = Vec::new();
    for segment in &segments {
        let Some(main_match) = main
            .find(segment)
            .map_err(|_| CodeScanError::EngineResource)?
        else {
            continue;
        };
        let mut satisfied = false;
        for target in &target_patterns {
            let Some(hit) = target
                .find(segment)
                .map_err(|_| CodeScanError::EngineResource)?
            else {
                continue;
            };
            if hit.start() > main_match.start() {
                satisfied = true;
                break;
            }
        }
        if satisfied {
            evidence.push(segment.trim().to_owned());
        }
    }
    Ok(evidence)
}

/// Runs every rule against `code` and returns one finding per matching rule.
///
/// Rules are evaluated in the order given, and a matching rule contributes
/// exactly one finding whose evidence holds all of its matches.
///
/// A rule whose `target_regexes` is present but empty is treated as a plain
/// rule: V1 gates the segment path on Python truthiness, where an empty list is
/// false, so an empty list must not switch on segment matching here either.
///
/// # Errors
/// Returns [`CodeScanError::RegexCompile`] for a pattern the engine rejects, or
/// [`CodeScanError::EngineResource`] if a match exhausts backtracking.
pub fn run_regex_rules(
    code: &str,
    rules: &[RuleDefinition],
    language: Language,
) -> Result<Vec<Finding>, CodeScanError> {
    let mut findings = Vec::new();
    for rule in rules {
        let evidence = match &rule.target_regexes {
            Some(targets) if !targets.is_empty() => {
                match_with_targets(code, rule, targets, language)?
            }
            _ => {
                let pattern = compile(&rule.regex, &rule.rule_id, code.len())?;
                let mut matches = Vec::new();
                for found in pattern.find_iter(code) {
                    let text = found.map_err(|_| CodeScanError::EngineResource)?;
                    matches.push(text.as_str().to_owned());
                }
                matches
            }
        };
        if evidence.is_empty() {
            continue;
        }
        findings.push(Finding {
            rule_id: rule.rule_id.clone(),
            severity: rule.severity,
            desc_zh: rule.desc_zh.clone(),
            desc_en: rule.desc_en.clone(),
            evidence,
        });
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Severity;

    fn plain_rule(rule_id: &str, regex: &str, severity: Severity) -> RuleDefinition {
        RuleDefinition {
            rule_id: rule_id.to_owned(),
            cwe_id: "CWE-000".to_owned(),
            desc_en: "en".to_owned(),
            desc_zh: "zh".to_owned(),
            regex: regex.to_owned(),
            severity,
            target_regexes: None,
        }
    }

    #[test]
    fn plain_rule_reports_every_match_as_evidence() {
        let rules = [plain_rule("digits", r"\d+", Severity::Warn)];
        let findings = run_regex_rules("a1 b22 c333", &rules, Language::Bash).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].evidence, ["1", "22", "333"]);
    }

    #[test]
    fn rule_with_no_match_produces_no_finding() {
        let rules = [plain_rule("never", r"zzz", Severity::Warn)];
        assert!(
            run_regex_rules("abc", &rules, Language::Bash)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_target_list_falls_back_to_plain_matching() {
        // Python treats `if []` as false, so an empty target list must scan the
        // whole input with the main pattern rather than by segment.
        let mut rule = plain_rule("empty-targets", r"rm", Severity::Deny);
        rule.target_regexes = Some(Vec::new());
        let findings = run_regex_rules("rm -rf /", &[rule], Language::Bash).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].evidence, ["rm"]);
    }

    #[test]
    fn target_rule_requires_a_target_after_the_main_match_in_one_segment() {
        // Real target rules pair a read command with a sensitive path, as in
        // shell-read-sensitive-file. Only the segment where the path follows the
        // command is evidence; `;` splits the surrounding commands away.
        let mut rule = plain_rule("read-sensitive", r"\bcat\b", Severity::Warn);
        rule.target_regexes = Some(vec![r"/etc/shadow".to_owned()]);
        let code = "ls -la ; cat /etc/shadow ; echo done";
        let findings = run_regex_rules(code, &[rule], Language::Bash).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].evidence, ["cat /etc/shadow"]);
    }

    #[test]
    fn target_before_main_in_segment_does_not_match() {
        // The sensitive path precedes the command in the same segment, so V1's
        // `target.start > main.start` gate rejects it.
        let mut rule = plain_rule("read-sensitive", r"\bcat\b", Severity::Warn);
        rule.target_regexes = Some(vec![r"/etc/shadow".to_owned()]);
        let findings = run_regex_rules("/etc/shadow cat", &[rule], Language::Bash).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn python_parenthesised_newlines_keep_a_call_in_one_segment() {
        // The newline inside `open(...)` would otherwise split the call away
        // from its argument; normalization keeps them together for Python.
        let mut rule = plain_rule("open-shadow", r"open", Severity::Deny);
        rule.target_regexes = Some(vec![r"/etc/shadow".to_owned()]);
        let code = "open(\n    '/etc/shadow'\n)";
        let findings = run_regex_rules(code, &[rule], Language::Python).unwrap();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn backtrack_budget_scales_then_caps_for_rpc_sized_input() {
        assert_eq!(backtrack_limit(0), MIN_BACKTRACK_LIMIT);
        assert_eq!(backtrack_limit(100_000), 12_800_000);
        assert_eq!(backtrack_limit(usize::MAX), MAX_BACKTRACK_LIMIT);
    }

    #[test]
    fn unusable_pattern_is_reported_against_its_rule() {
        // An unbalanced group is rejected at compile time.
        let rules = [plain_rule("broken", r"(", Severity::Warn)];
        assert_eq!(
            run_regex_rules("x", &rules, Language::Bash),
            Err(CodeScanError::RegexCompile("broken".to_owned()))
        );
    }
}
