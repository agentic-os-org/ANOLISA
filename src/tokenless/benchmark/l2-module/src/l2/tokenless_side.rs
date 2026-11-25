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

//! Direct component calls for reference tests only; `l2_compare` does not use them and they
//! are not the production final state. JSON keeps numeric literals; new domains get raw text.
//!
//! Latency basis: **in-process** (`Instant` around the compress call only).

use crate::l2::{Category, L2Error};
use serde_json::{Value, json};
use std::time::Instant;
use tokenless_compressors::{
    BuildLogCompressor, HtmlExtractor, JsonCompressionContext, JsonCompressor, Recoverability,
    RecoveryMethod, SearchResultsCompressor, TabularCompressor,
};

/// Latency-basis label stamped on every tokenless-side result row.
pub const LATENCY_BASIS: &str = "in-process";

/// Output of one in-process compression call.
#[derive(Debug, Clone)]
pub struct TokenlessOutput {
    /// Actual candidate wire text: serialized JSON, or unwrapped output for content domains.
    pub compressed: String,
    /// Text that retention checks run against. For JSON samples this is the
    /// wire form; for wrapped text (source code) it is the inner `content`
    /// string, un-escaped, so a ground-truth literal containing a quote or a
    /// newline matches the way it was written rather than its JSON-escaped
    /// serialization — escaping a `"` into `\"` was scoring content that was
    /// fully present as a spurious retention miss.
    pub retention_text: String,
    /// Pure compression time in seconds (serialization excluded).
    pub latency_s: f64,
    /// The no-recovery mode rejects unrecoverable candidates outside the component.
    pub rejected_unrecoverable: bool,
}

/// Parses a `json`-category sample, rejecting a top-level JSON string.
///
/// `JsonCompressor` unwraps an input whose top level is a string carrying a
/// parseable object or array and compresses the inner value. Its output would
/// then be counted against that inner value while [`wire_before`] still reports
/// the quoted form, putting the two sides of the compression rate on different
/// bases and shifting what retention asserts against. No committed sample is
/// shaped that way, and the engine only unwraps when the inner text parses as
/// an object or array; this rejects every top-level string rather than
/// restating that condition, so the guard cannot drift out of step with the
/// engine — and a `json` sample that is a bare string measures string escaping
/// rather than JSON compression in any case.
fn parse_json_sample(content: &str) -> Result<Value, L2Error> {
    let value: Value = serde_json::from_str(content)
        .map_err(|e| L2Error::InvalidSample(format!("json sample is not valid JSON: {e}")))?;
    if value.is_string() {
        return Err(L2Error::InvalidSample(
            "json sample has a string at its top level: the compressor may unwrap it and \
             be measured against a different value than the before-count reports"
                .to_string(),
        ));
    }
    Ok(value)
}

/// Produces a domain component candidate without Runtime arbitration.
///
/// JSON is validated, not re-rendered; build-log, tabular, search and HTML call components directly.
/// Other compatibility calls use a `{"content": text}` envelope counted on both sides.
/// HTML measures the rendered candidate only, not Runtime capability, recovery or savings gates.
///
/// # Errors
///
/// Returns [`L2Error::InvalidSample`] when a `json`-category sample fails to
/// parse, carries a string at its top level, or the compressor rejects the
/// input, and [`L2Error::Json`] if the sample cannot be serialized into the
/// payload handed to the engine.
pub fn compress(category: Category, content: &str) -> Result<TokenlessOutput, L2Error> {
    if matches!(
        category,
        Category::BuildLog | Category::Tabular | Category::Search | Category::Html
    ) {
        return compress_content_domain(category, content);
    }
    let input = if category == Category::Json {
        parse_json_sample(content)?;
        content.to_owned()
    } else {
        serde_json::to_string(&json!({ "content": content }))?
    };
    let compressor = JsonCompressor::default();
    // Matches L1's default CLI entry point; with no Stash, this does not
    // affect output or compression metrics.
    let recovery = RecoveryMethod::Shell;
    let context = JsonCompressionContext {
        recovery: &recovery,
        stash: None,
        allow_toon: false,
        min_toon_chars: usize::MAX,
        allow_unrecoverable: false,
        embedded_html: false,
    };
    // Time only the compress call: the envelope handling below is measurement
    // plumbing, not engine work.
    let start = Instant::now();
    let outcome = compressor
        .compress(&input, &context)
        .map_err(|error| L2Error::InvalidSample(error.to_string()))?;
    let latency_s = start.elapsed().as_secs_f64();

    let rejected_unrecoverable = outcome.recoverability == Recoverability::Unrecoverable;
    let compressed = if rejected_unrecoverable {
        input
    } else {
        outcome.output
    };
    // Retention matches literal source substrings, so for the wrapped-text
    // envelope it must see the inner string, not its JSON-escaped form. JSON
    // samples keep the wire form: their ground truth is written to match it.
    //
    // The engine returns the wire form, so the envelope is re-parsed here to
    // reach that inner string. Anything that is not a `{"content": string}`
    // object falls back to the wire form rather than to an empty haystack,
    // which would score every ground-truth item as lost and report a harness
    // fault as a product defect.
    let retention_text = if category == Category::Json {
        compressed.clone()
    } else {
        let envelope = serde_json::from_str::<Value>(&compressed).ok();
        envelope
            .as_ref()
            .and_then(|value| value.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| compressed.clone())
    };

    Ok(TokenlessOutput {
        compressed,
        retention_text,
        latency_s,
        rejected_unrecoverable,
    })
}

/// The wire form the tokenless side counts "before" tokens on.
///
/// Returns raw text for JSON and direct domains, or the exact envelope given to the component.
/// Both counts use actual wire text, so re-rendering or envelope overhead is never savings.
///
/// # Errors
///
/// Same failure modes as [`compress`]: both sides must agree on the value they
/// count, so the top-level-string rejection applies here too.
pub fn wire_before(category: Category, content: &str) -> Result<String, L2Error> {
    if category == Category::Json {
        parse_json_sample(content)?;
        return Ok(content.to_owned());
    }
    if matches!(
        category,
        Category::BuildLog | Category::Tabular | Category::Search | Category::Html
    ) {
        return Ok(content.to_owned());
    }
    Ok(serde_json::to_string(&json!({ "content": content }))?)
}

fn compress_content_domain(category: Category, content: &str) -> Result<TokenlessOutput, L2Error> {
    let start = Instant::now();
    let compressed = match category {
        Category::BuildLog => BuildLogCompressor.compress(content, None).output,
        Category::Tabular => TabularCompressor.compress(content).output,
        Category::Search => SearchResultsCompressor
            .compress_paths(content)
            .or_else(|| SearchResultsCompressor.compress(content))
            .unwrap_or_else(|| content.to_owned()),
        Category::Html => HtmlExtractor
            .render(content)
            .map(|view| view.output)
            .unwrap_or_else(|| content.to_owned()),
        _ => {
            return Err(L2Error::InvalidSample(format!(
                "unsupported content-domain category {category}"
            )));
        }
    };
    Ok(TokenlessOutput {
        latency_s: start.elapsed().as_secs_f64(),
        retention_text: compressed.clone(),
        compressed,
        rejected_unrecoverable: false,
    })
}
