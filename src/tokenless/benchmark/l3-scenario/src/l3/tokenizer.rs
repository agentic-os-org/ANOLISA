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

//! Token counting for the L3 comparison.
//!
//! Counts come from a real BPE encoder, never from a bytes/4 heuristic: a
//! compressor that rewrites structure changes how text tokenizes, so an
//! estimate can move in the opposite direction from the truth. `o200k_base` is
//! the headline and `cl100k_base` is reported alongside it as a
//! tokenizer-sensitivity check — the same two encodings L2 uses, so L2 and L3
//! numbers sit on one scale.

use tiktoken_rs::CoreBPE;

use super::L3Error;

/// The two encodings every measurement is reported under.
pub struct Tokenizers {
    /// Headline encoding.
    pub o200k: CoreBPE,
    /// Cross-check encoding.
    pub cl100k: CoreBPE,
}

/// Token counts of one payload under both encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenCount {
    /// Count under `o200k_base`.
    pub o200k: usize,
    /// Count under `cl100k_base`.
    pub cl100k: usize,
}

impl std::ops::Add for TokenCount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            o200k: self.o200k + rhs.o200k,
            cl100k: self.cl100k + rhs.cl100k,
        }
    }
}

impl std::iter::Sum for TokenCount {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |acc, c| acc + c)
    }
}

impl Tokenizers {
    /// Load both encodings.
    ///
    /// # Errors
    ///
    /// Returns [`L3Error::Tokenizer`] if either encoding cannot be constructed.
    pub fn load() -> Result<Self, L3Error> {
        let o200k = tiktoken_rs::o200k_base().map_err(|e| L3Error::Tokenizer {
            name: "o200k_base".to_string(),
            message: e.to_string(),
        })?;
        let cl100k = tiktoken_rs::cl100k_base().map_err(|e| L3Error::Tokenizer {
            name: "cl100k_base".to_string(),
            message: e.to_string(),
        })?;
        Ok(Self { o200k, cl100k })
    }

    /// Count one string under both encodings.
    pub fn count(&self, text: &str) -> TokenCount {
        TokenCount {
            o200k: self.o200k.encode_with_special_tokens(text).len(),
            cl100k: self.cl100k.encode_with_special_tokens(text).len(),
        }
    }

    /// Count a JSON value as the compact JSON an agent would actually send.
    ///
    /// Serialization failure is impossible for a `Value` that came from parsing,
    /// so it falls back to an empty count rather than propagating an error that
    /// callers could not act on.
    pub fn count_value(&self, value: &serde_json::Value) -> TokenCount {
        serde_json::to_string(value)
            .map(|s| self.count(&s))
            .unwrap_or_default()
    }
}

/// Compression rate `1 - after/before`, or `None` when there was nothing to
/// compress.
///
/// Returning `None` for an empty payload keeps a degenerate 0/0 out of the
/// statistics instead of recording it as a real 0% observation.
pub fn compression_rate(before: usize, after: usize) -> Option<f64> {
    if before == 0 {
        return None;
    }
    Some(1.0 - (after as f64 / before as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_is_none_for_empty_input() {
        assert_eq!(compression_rate(0, 0), None);
    }

    #[test]
    fn rate_matches_definition() {
        assert_eq!(compression_rate(100, 25), Some(0.75));
        assert_eq!(compression_rate(100, 100), Some(0.0));
    }

    #[test]
    fn rate_is_negative_when_payload_grows() {
        // The reference's CacheAligner can add markers; its own RAG test tolerates a
        // 1% increase. Growth must surface as a negative rate, not clamp to 0.
        let rate = compression_rate(100, 110).expect("non-empty input");
        assert!(rate < 0.0, "expected negative rate, got {rate}");
    }

    #[test]
    fn both_encodings_count_nonzero() {
        let tk = Tokenizers::load().expect("tokenizers load");
        let c = tk.count("the quick brown fox jumps over the lazy dog");
        assert!(c.o200k > 0 && c.cl100k > 0);
    }

    #[test]
    fn counts_sum_across_messages() {
        let tk = Tokenizers::load().expect("tokenizers load");
        let a = tk.count("hello");
        let b = tk.count("world");
        let total: TokenCount = [a, b].into_iter().sum();
        assert_eq!(total.o200k, a.o200k + b.o200k);
        assert_eq!(total.cl100k, a.cl100k + b.cl100k);
    }
}
