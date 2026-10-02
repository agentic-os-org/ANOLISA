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

//! Real-tokenizer counting via tiktoken-rs.
//!
//! `o200k_base` is the headline base (GPT-4o/o-series family); `cl100k_base`
//! is reported alongside because headroom's own counters historically use it,
//! so publishing both makes the cross-check against the worker's
//! `hr_tokens_*` fields meaningful.

use crate::l2::L2Error;
use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

/// Token counts for one text under both reported bases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenCounts {
    /// o200k_base count — the headline metric.
    pub o200k: usize,
    /// cl100k_base count — side report for tokenizer sensitivity.
    pub cl100k: usize,
}

// BPE construction loads embedded rank tables (tens of ms), so both encoders
// are built once and cached for the process lifetime. The inner Result is
// cached too: a failed init stays failed, and every caller sees the same
// error instead of retrying a doomed construction.
fn o200k() -> Result<&'static CoreBPE, L2Error> {
    static CACHE: OnceLock<Result<CoreBPE, String>> = OnceLock::new();
    CACHE
        .get_or_init(|| tiktoken_rs::o200k_base().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| L2Error::Tokenizer(format!("o200k_base: {e}")))
}

fn cl100k() -> Result<&'static CoreBPE, L2Error> {
    static CACHE: OnceLock<Result<CoreBPE, String>> = OnceLock::new();
    CACHE
        .get_or_init(|| tiktoken_rs::cl100k_base().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| L2Error::Tokenizer(format!("cl100k_base: {e}")))
}

/// Counts `text` under both bases.
///
/// Uses `encode_ordinary` (no special-token handling) because payloads are
/// plain tool output, and treating accidental `<|...|>` sequences as special
/// tokens would silently skew counts.
///
/// # Errors
///
/// Returns [`L2Error::Tokenizer`] when an encoder cannot be initialised.
pub fn count(text: &str) -> Result<TokenCounts, L2Error> {
    Ok(TokenCounts {
        o200k: o200k()?.encode_ordinary(text).len(),
        cl100k: cl100k()?.encode_ordinary(text).len(),
    })
}
