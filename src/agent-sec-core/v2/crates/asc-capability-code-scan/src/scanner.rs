//! Scan orchestration: the single entry point that turns code into a verdict.
//!
//! A direct port of V1's `scanner.scan`. It validates input, rewrites a Bash
//! command that wraps an inline interpreter call into that call's own language,
//! optionally narrows the rule set, runs the regex engine, and assembles a
//! [`ScanResult`]. Every failure is turned into an error result rather than
//! propagated, because the caller must always receive a verdict.

use std::borrow::Cow;
use std::time::Instant;

use crate::errors::CodeScanError;
use crate::extractor::extract_inline_code;
use crate::findings::{Finding, Verdict, build_summary, compute_verdict};
use crate::rules::{Language, load_rules};
use crate::run_regex_rules;

/// Engine version reported in every result.
///
/// V1 reports the `agent-sec-cli` package version; the V2 scanner runs inside
/// the daemon, so it reports the daemon workspace version instead. The value
/// legitimately differs from V1 — the goldens only require it to be present and
/// not the literal `"unknown"`.
const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The mode literal that selects the LLM engine.
///
/// Dispatch is exact-match, exactly as V1: only this literal selects the LLM
/// path, and every other value — a misspelling, a different case — falls
/// through to regex.
const LLM_MODE: &str = "llm";

/// Outcome of one scan, serialized to the wire contract V1 established.
///
/// Field order is the pydantic model's declaration order and is asserted by the
/// end-to-end goldens: `ok, verdict, summary, findings, language,
/// engine_version, elapsed_ms`. `serde_json` preserves struct field order and
/// leaves non-ASCII unescaped, so `desc_zh` stays raw UTF-8 as the goldens
/// require.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScanResult {
    /// Whether the scan itself succeeded; `false` only for an error result.
    pub ok: bool,
    /// Highest-severity outcome across findings, or [`Verdict::Error`].
    pub verdict: Verdict,
    /// Human-readable one-line summary; wording is part of the contract.
    pub summary: String,
    /// Every matching rule, in rule-load order; empty on pass or error.
    pub findings: Vec<Finding>,
    /// Language the code was scanned as, after any inline rewrite.
    pub language: Language,
    /// Version of the engine that produced this result.
    pub engine_version: &'static str,
    /// Wall-clock scan duration in whole milliseconds.
    pub elapsed_ms: u64,
}

impl ScanResult {
    /// Builds the error result for a failed scan.
    ///
    /// Mirrors V1's `_error_result`: `ok` is false, the verdict is
    /// [`Verdict::Error`], the summary is `scan error: {message}`, and no
    /// findings are carried. `language` is whatever the caller was scanning as
    /// when the failure occurred.
    fn error(language: Language, elapsed_ms: u64, error: &CodeScanError) -> Self {
        Self {
            ok: false,
            verdict: Verdict::Error,
            summary: format!("scan error: {error}"),
            findings: Vec::new(),
            language,
            engine_version: ENGINE_VERSION,
            elapsed_ms,
        }
    }
}

/// Scans `code` written in `language` for security issues.
///
/// This is the sole public entry point of the capability. When `rules` is
/// `Some`, only rules whose id appears in it are run; `None` runs the whole set
/// for the language. `mode` selects the engine: only the literal `"llm"` asks
/// for the LLM engine, which this crate does not ship, so that request returns
/// an error result; any other value runs the regex engine.
///
/// Never returns `Err`: every failure is folded into an error [`ScanResult`],
/// because the caller must always be able to act on a verdict.
pub fn scan(code: &str, language: Language, rules: Option<&[String]>, mode: &str) -> ScanResult {
    let start = Instant::now();

    // V1 reports elapsed_ms = 0 for empty input, before the clock is consulted.
    if code.trim().is_empty() {
        return ScanResult::error(language, 0, &CodeScanError::InputEmpty);
    }

    if mode == LLM_MODE {
        return ScanResult::error(language, elapsed_ms(start), &CodeScanError::LlmUnavailable);
    }

    // Resolved before the pipeline runs so that a failure reports the language
    // actually scanned. V1 rebinds its `code`/`language` locals inside the try
    // block, so its error handler already sees the rewritten language.
    let (code, language) = resolve_target(code, language);

    match scan_with_regex(&code, language, rules) {
        Ok(findings) => {
            let verdict = compute_verdict(&findings);
            let summary = build_summary(&findings, language);
            ScanResult {
                ok: true,
                verdict,
                summary,
                findings,
                language,
                engine_version: ENGINE_VERSION,
                elapsed_ms: elapsed_ms(start),
            }
        }
        Err(error) => ScanResult::error(language, elapsed_ms(start), &error),
    }
}

/// Returns the code and language that will actually be scanned.
///
/// A Bash command may wrap an inline interpreter call; rewriting code and
/// language together is what makes a python-in-bash snippet report
/// `language = python`. Nested interpreters are out of scope, matching V1.
///
/// Kept separate from the pipeline so the success and error paths cannot
/// disagree about the language.
fn resolve_target(code: &str, language: Language) -> (Cow<'_, str>, Language) {
    if language == Language::Bash
        && let Some((inline_code, inline_language)) = extract_inline_code(code)
    {
        return (Cow::Owned(inline_code), inline_language);
    }
    (Cow::Borrowed(code), language)
}

/// Loads the rule set for `language`, narrows it, and runs the regex engine.
///
/// # Errors
/// Propagates the first rule- or engine-layer failure so [`scan`] can render it
/// as an error result.
fn scan_with_regex(
    code: &str,
    language: Language,
    rules: Option<&[String]>,
) -> Result<Vec<Finding>, CodeScanError> {
    let mut rule_set = load_rules(language)?;
    if let Some(enabled) = rules {
        rule_set.retain(|rule| enabled.iter().any(|id| id == &rule.rule_id));
    }
    run_regex_rules(code, &rule_set, language)
}

/// Whole milliseconds elapsed since `start`, saturating rather than wrapping.
fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_code_passes_with_no_findings() {
        let result = scan("echo hello", Language::Bash, None, "regex");
        assert!(result.ok);
        assert_eq!(result.verdict, Verdict::Pass);
        assert!(result.findings.is_empty());
        assert_eq!(result.summary, "No issues found in bash code");
        assert_eq!(result.language, Language::Bash);
    }

    #[test]
    fn dangerous_code_reports_a_finding_and_warns() {
        let result = scan("rm -rf /tmp/test", Language::Bash, None, "regex");
        assert!(result.ok);
        assert_eq!(result.verdict, Verdict::Warn);
        assert!(!result.findings.is_empty());
    }

    #[test]
    fn empty_input_is_an_error_with_zero_elapsed() {
        for blank in ["", "   ", "\n\t "] {
            let result = scan(blank, Language::Python, None, "regex");
            assert!(!result.ok);
            assert_eq!(result.verdict, Verdict::Error);
            assert_eq!(result.summary, "scan error: empty input code");
            assert_eq!(result.elapsed_ms, 0);
            assert_eq!(result.language, Language::Python);
        }
    }

    #[test]
    fn inline_rewrite_resolves_the_language_before_the_pipeline_runs() {
        // V1 rebinds its code/language locals before entering the pipeline, so
        // an error result carries the rewritten language. Resolve once before
        // either the success or error path for the same contract in V2.
        let (code, language) = resolve_target(
            r#"python3 -c "import os; os.system('rm -rf /')""#,
            Language::Bash,
        );
        assert_eq!(language, Language::Python);
        assert_eq!(code, "import os; os.system('rm -rf /')");

        let (code, language) = resolve_target("rm -rf /tmp/test", Language::Bash);
        assert_eq!(language, Language::Bash);
        assert!(matches!(code, Cow::Borrowed(_)));

        let (_, language) = resolve_target("bash -c 'rm -rf /'", Language::Python);
        assert_eq!(language, Language::Python);
    }

    #[test]
    fn llm_mode_is_unavailable_in_this_build() {
        let result = scan("echo hello", Language::Bash, None, LLM_MODE);
        assert!(!result.ok);
        assert_eq!(result.verdict, Verdict::Error);
        assert_eq!(result.summary, "scan error: LLM model not available");
    }

    #[test]
    fn unknown_mode_falls_through_to_regex() {
        // Only the literal "llm" selects the LLM path; a typo runs regex.
        let typo = scan("rm -rf /tmp/test", Language::Bash, None, "Llm");
        let regex = scan("rm -rf /tmp/test", Language::Bash, None, "regex");
        assert_eq!(typo.ok, regex.ok);
        assert_eq!(typo.verdict, regex.verdict);
        assert_eq!(typo.findings, regex.findings);
    }

    #[test]
    fn inline_python_in_bash_reports_python() {
        // Extraction swaps code and language together before rules are loaded,
        // so a python snippet wrapped in bash is scanned and reported as python.
        let result = scan(
            r#"python3 -c "import os; os.system('rm -rf /')""#,
            Language::Bash,
            None,
            "regex",
        );
        assert_eq!(result.language, Language::Python);
    }

    #[test]
    fn rule_filter_narrows_the_active_set() {
        let all = scan("rm -rf /tmp/test", Language::Bash, None, "regex");
        let matched = all
            .findings
            .first()
            .expect("a finding exists")
            .rule_id
            .clone();
        let narrowed = scan(
            "rm -rf /tmp/test",
            Language::Bash,
            Some(std::slice::from_ref(&matched)),
            "regex",
        );
        assert!(narrowed.findings.iter().all(|f| f.rule_id == matched));

        let empty = scan("rm -rf /tmp/test", Language::Bash, Some(&[]), "regex");
        assert!(empty.ok);
        assert_eq!(empty.verdict, Verdict::Pass);
        assert!(empty.findings.is_empty());
    }

    #[test]
    fn large_multiline_input_scans_the_tail_without_resource_exhaustion() {
        let filler = (0..4_000)
            .map(|index| format!("echo line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = scan(
            &format!("{filler}\nrm -rf /tmp/test"),
            Language::Bash,
            None,
            "regex",
        );
        assert!(result.ok, "{}", result.summary);
        assert!(
            result
                .findings
                .iter()
                .any(|finding| finding.rule_id == "shell-recursive-delete"),
            "tail rule was not reported: {:?}",
            result.findings
        );
    }

    #[test]
    fn result_serializes_in_v1_field_order_without_escaping_utf8() {
        // The whole JSON contract in one place: top-level key order, finding
        // key order, two-space indent, and raw UTF-8 for desc_zh.
        let result = scan("rm -rf /tmp/test", Language::Bash, None, "regex");
        let json = serde_json::to_string_pretty(&result).expect("result serializes");

        let top_keys: Vec<&str> = [
            "\"ok\"",
            "\"verdict\"",
            "\"summary\"",
            "\"findings\"",
            "\"language\"",
            "\"engine_version\"",
            "\"elapsed_ms\"",
        ]
        .into_iter()
        .filter(|key| json.contains(key))
        .collect();
        assert_eq!(top_keys.len(), 7, "a top-level key is missing: {json}");
        // Order check: each key appears after the previous one.
        let mut cursor = 0;
        for key in [
            "\"ok\"",
            "\"verdict\"",
            "\"summary\"",
            "\"findings\"",
            "\"language\"",
            "\"engine_version\"",
            "\"elapsed_ms\"",
        ] {
            let at = json.find(key).expect("key present");
            assert!(at >= cursor, "key {key} is out of order");
            cursor = at;
        }

        // desc_zh stays raw UTF-8 rather than \uXXXX escapes.
        assert!(!json.contains("\\u"), "non-ASCII was escaped: {json}");
        assert!(json.contains("递归删除"), "expected raw Chinese in {json}");

        // Two-space indent, first key on its own line.
        assert!(
            json.starts_with("{\n  \"ok\":"),
            "unexpected layout: {json}"
        );
    }
}
