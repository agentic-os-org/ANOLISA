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
/// A literal that is not found verbatim is retried on the normalized form of
/// both texts ([`normalize_format`]): Markdown escaping (`required\_heading`)
/// and re-wrapped whitespace change the format, not the content, and must not
/// score as a loss. Content that is actually gone still fails.
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
                    || normalized
                        .get_or_insert_with(|| normalize_format(compressed))
                        .contains(normalize_format(s).as_str());
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

/// 校验 grep 的文件、行号与命中内容三元组，避免路径共享格式造成假丢失。
///
/// # Errors
/// truth 的正则表达式无效时返回 [`L2Error::Regex`]。
pub fn check_search(
    ground_truth: &[GroundTruth],
    compressed: &str,
) -> Result<RetentionResult, L2Error> {
    let normalized = normalized_search_records(compressed);
    let records: std::collections::HashSet<String> =
        normalized.lines().map(normalize_format).collect();
    let mut passed = 0;
    let mut failures = Vec::new();
    for item in ground_truth {
        let retained = match item {
            GroundTruth::Substring(record) => records.contains(&normalize_format(record)),
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

/// 还原明确的 path/line/body 绑定，仅用于事实校验，不改变 Token 计数的原始输出。
/// 支持原始 grep 行、Tokenless 的 JSON File header 和 Headroom 的完整路径 header。
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
