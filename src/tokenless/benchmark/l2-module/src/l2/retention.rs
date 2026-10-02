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
    /// Fraction of items retained; 1.0 when there was nothing to check —
    /// an empty ground truth must not count against a compressor.
    pub fn rate(&self) -> f64 {
        if self.total == 0 {
            return 1.0;
        }
        self.passed as f64 / self.total as f64
    }
}

/// Checks every ground-truth item against `compressed`.
///
/// Substrings are matched case-sensitively (facts like ids and error codes
/// are case-significant); regex items are compiled fresh per call — the item
/// counts are tiny, so caching compiled regexes is not worth the ownership
/// complexity.
///
/// # Errors
///
/// Returns [`L2Error::Regex`] when a ground-truth pattern fails to compile —
/// that is an asset bug and must surface, not score as a retention miss.
pub fn check(ground_truth: &[GroundTruth], compressed: &str) -> Result<RetentionResult, L2Error> {
    let mut passed = 0usize;
    let mut failures = Vec::new();
    for item in ground_truth {
        match item {
            GroundTruth::Substring(s) => {
                if compressed.contains(s.as_str()) {
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
