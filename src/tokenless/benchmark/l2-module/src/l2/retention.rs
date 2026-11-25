// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Ground-truth retention checking: does the compressed payload still
//! contain the facts an agent would need?
//!
//! Retention is checked item-by-item (not all-or-nothing) so the report can
//! show *which* facts each compressor dropped, and Wilson intervals over the
//! item counts stay meaningful.

use crate::l2::{GroundTruth, L2Error};
use regex::Regex;

/// The result of checking one compressed text against its ground truth.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RetentionResult {
    /// Items still present after compression.
    pub passed: usize,
    /// Total items checked.
    pub total: usize,
    /// Human-readable descriptions of the items that were lost.
    pub failures: Vec<String>,
}

impl RetentionResult {
    /// Fraction retained, or None when no fact was actually checked.
    pub fn rate(&self) -> Option<f64> {
        (self.total > 0).then(|| self.passed as f64 / self.total as f64)
    }
}

/// Checks every ground-truth item against `compressed`.
///
/// Substrings are matched case-sensitively (facts like ids and error codes
/// are case-significant); regex items are compiled fresh per call — the item
/// counts are tiny, so caching compiled regexes is not worth the ownership
/// complexity.
///
/// After a verbatim miss, ordinary literals are retried with Markdown escapes
/// removed and whitespace collapsed. Literals containing a backslash or repeated
/// whitespace stay exact; [`check_search`] keeps grep path/line identity separately.
///
/// # Errors
///
/// Returns [`L2Error::Regex`] when a ground-truth pattern fails to compile —
/// that is an asset bug and must surface, not score as a retention miss.
pub fn check(ground_truth: &[GroundTruth], compressed: &str) -> Result<RetentionResult, L2Error> {
    let mut passed = 0usize;
    let mut failures = Vec::new();
    let mut normalized: Option<String> = None;
    for item in ground_truth {
        match item {
            GroundTruth::Substring(s) => {
                let found = compressed.contains(s.as_str())
                    || (!s.contains('\\')
                        && !s
                            .chars()
                            .zip(s.chars().skip(1))
                            .any(|(left, right)| left.is_whitespace() && right.is_whitespace())
                        && normalized
                            .get_or_insert_with(|| normalize_format(compressed))
                            .contains(normalize_format(s).as_str()));
                if found {
                    passed += 1;
                } else {
                    failures.push(format!("substring not retained: {s:?}"));
                }
            }
            GroundTruth::Pattern { regex } => {
                let re = Regex::new(regex)?;
                if re.is_match(compressed) {
                    passed += 1;
                } else {
                    failures.push(format!("regex not matched: {regex:?}"));
                }
            }
        }
    }
    Ok(RetentionResult {
        passed,
        total: ground_truth.len(),
        failures,
    })
}

/// Format-only normalization for fact matching: Markdown backslash escapes are
/// removed and whitespace runs collapse to one space. Case and every other
/// character are kept, so no content can appear that was not there.
pub fn normalize_format(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\'
            && chars
                .peek()
                .is_some_and(|next| "\\`*_{}[]()#+-.!|<>~".contains(*next))
        {
            continue;
        }
        unescaped.push(character);
    }
    unescaped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Checks grep facts as path/line/body triples, so shared path headers are not losses.
///
/// # Errors
/// Returns [`L2Error::Regex`] when a ground-truth pattern is invalid.
pub fn check_search(
    ground_truth: &[GroundTruth],
    compressed: &str,
) -> Result<RetentionResult, L2Error> {
    let normalized = normalized_search_records(compressed);
    let records: std::collections::HashSet<String> =
        normalized.lines().map(normalize_search_fact).collect();
    let mut passed = 0;
    let mut failures = Vec::new();
    for item in ground_truth {
        let retained = match item {
            GroundTruth::Substring(record) => records.contains(&normalize_search_fact(record)),
            GroundTruth::Pattern { regex } => Regex::new(regex)?.is_match(&normalized),
        };
        if retained {
            passed += 1;
        } else {
            failures.push(format!("search record not retained: {item:?}"));
        }
    }
    Ok(RetentionResult {
        passed,
        total: ground_truth.len(),
        failures,
    })
}

fn normalize_search_fact(record: &str) -> String {
    match record.splitn(3, ':').collect::<Vec<_>>().as_slice() {
        [path, line, body] => format!("{path}:{line}:{}", normalize_format(body)),
        _ => record.to_owned(),
    }
}

/// Restores explicit path/line/body bindings for fact checks only, never token counts.
/// Accepts raw grep lines, Tokenless JSON `File=` headers and Headroom full-path headers.
pub fn normalized_search_records(text: &str) -> String {
    let mut output = String::new();
    let mut header: Option<String> = None;
    for line in text.lines() {
        if let Some(quoted) = line.strip_prefix("File=") {
            header = serde_json::from_str::<String>(quoted).ok();
            continue;
        }
        if let Some((path, suffix)) = line.split_once(':')
            && let Some((number, body)) = suffix.split_once(':')
            && !number.is_empty()
            && number.bytes().all(|value| value.is_ascii_digit())
            && (path.contains('/') || path.contains('.'))
        {
            let bound_path = match &header {
                Some(prefix) if prefix.ends_with('/') && !path.contains('/') => {
                    format!("{prefix}{path}")
                }
                _ => path.to_owned(),
            };
            output.push_str(&format!("{bound_path}:{number}:{}\n", body.trim()));
            continue;
        }
        if let Some((number, body)) = line.split_once(':')
            && !number.is_empty()
            && number.bytes().all(|value| value.is_ascii_digit())
            && let Some(path) = &header
        {
            output.push_str(&format!("{path}:{number}:{}\n", body.trim()));
            continue;
        }
        header = (!line.contains(':') && (line.contains('/') || line.contains('.')))
            .then(|| line.to_owned());
    }
    output
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn ordinary_facts_keep_markdown_and_wrapping_equivalence() {
        for (fact, output) in [
            ("service_01.rs", r"…/src/service\_01.rs"),
            ("benchmark_identity.json", r"- benchmark\_identity.json"),
            ("test result: FAILED", "test result:\nFAILED. 2 passed"),
            ("2026-11-24T11:00:00Z", "observed: 2026-11-24T11:00:00Z"),
        ] {
            let facts = vec![GroundTruth::Substring(fact.into())];
            assert_eq!(check(&facts, output).unwrap().passed, 1, "{fact}");
            assert_eq!(check(&facts, "unrelated text").unwrap().passed, 0);
        }
    }

    #[test]
    fn search_identity_and_raw_paths_are_not_markdown_equivalence() {
        for (expected, changed) in [
            (
                "src/a  b.rs:1:required match",
                "src/a b.rs:1:required match",
            ),
            (
                r"src/a\_b.rs:1:required match",
                "src/a_b.rs:1:required match",
            ),
        ] {
            let facts = vec![GroundTruth::Substring(expected.into())];
            assert_eq!(check_search(&facts, changed).unwrap().passed, 0);
            assert_eq!(check_search(&facts, expected).unwrap().passed, 1);
            assert_eq!(check(&facts, changed).unwrap().passed, 0);
        }
    }
}
