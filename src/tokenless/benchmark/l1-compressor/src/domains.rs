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

//! Content-domain measurements on shared synthetic inputs and a real tokenizer.

use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{Value, json};
use tiktoken_rs::CoreBPE;
use tokenless_ccr::{InMemoryStore, StashStore, compute_key};
use tokenless_compressors::{
    BuildLogCompressor, HtmlExtractor, JsonCompressionContext, JsonCompressor, RecoveryMethod,
    SearchResultsCompressor, TabularCompressor,
};
use tokenless_schema::SchemaCompressor;

/// Failures that prevent publishing a content-domain measurement.
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    /// The headline tokenizer could not initialize.
    #[error("tokenizer initialization failed: {0}")]
    Tokenizer(String),
    /// Controlled JSON could not be serialized or compressed.
    #[error("JSON fixture compression failed: {0}")]
    Json(String),
    /// Original recovery storage or verification failed.
    #[error("recovery verification failed: {0}")]
    Recovery(String),
    /// The original input fails its own ground-truth contract.
    #[error("invalid original fact contract: {0}")]
    InvalidFacts(String),
}

fn encoder() -> Result<&'static CoreBPE, DomainError> {
    static ENCODER: OnceLock<Result<CoreBPE, String>> = OnceLock::new();
    ENCODER
        .get_or_init(|| tiktoken_rs::o200k_base().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| DomainError::Tokenizer(error.clone()))
}

/// Count with the headline tokenizer; initialization failure never uses estimates.
pub fn count_text(input: &str) -> Result<usize, DomainError> {
    Ok(encoder()?.encode_ordinary(input).len())
}

// Bind each row to its fixture identity, condition and original bytes.
#[allow(clippy::too_many_arguments)]
fn row(
    id: &str,
    domain: &str,
    mode: &str,
    original: &str,
    output: &str,
    elapsed_ms: f64,
    checks: Value,
) -> Result<Value, DomainError> {
    let tokenizer = encoder()?;
    let before = tokenizer.encode_ordinary(original).len();
    let after = tokenizer.encode_ordinary(output).len();
    let savings = (before != 0).then(|| 1.0 - after as f64 / before as f64);
    Ok(json!({
        "sample_id": id,
        "domain": domain,
        "mode": mode,
        "payload_hash": compute_key(original.as_bytes()),
        "output_hash": compute_key(output.as_bytes()),
        "tokenizer": "o200k_base",
        "tokens_before": before,
        "tokens_after": after,
        "bytes_before": original.len(),
        "bytes_after": output.len(),
        "engine_estimate_before": original.len().div_ceil(4),
        "engine_estimate_after": output.len().div_ceil(4),
        "compression_rate": savings,
        "candidate_changed": original != output,
        "expanded": after > before,
        "latency_ms": elapsed_ms,
        "latency_basis": "in-process component call; tokenization excluded",
        "checks": checks,
    }))
}

/// Measure current component domains and negative controls without tools or models.
///
/// # Errors
/// Propagates tokenizer, controlled-JSON and recovery failures without zero substitutes.
pub fn domain_metrics() -> Result<Value, DomainError> {
    let _ = encoder()?;
    let mut rows = Vec::new();
    for (id, input) in [
        (
            "json-pretty",
            serde_json::to_string_pretty(&crate::response_canonical())
                .map_err(|error| DomainError::Json(error.to_string()))?,
        ),
        (
            "json-numeric-literals",
            "{\n  \"integer\": 9007199254740993,\n  \"decimal\": 0.12345678901234567890123456789,\n  \"negative_zero\": -0.0\n}".to_owned(),
        ),
    ] {
        let recovery = RecoveryMethod::None;
        let context = JsonCompressionContext {
            recovery: &recovery,
            stash: None,
            allow_toon: false,
            min_toon_chars: usize::MAX,
            allow_unrecoverable: false,
            embedded_html: false,
        };
        let start = Instant::now();
        let outcome = JsonCompressor::default()
            .compress(&input, &context)
            .map_err(|error| DomainError::Json(error.to_string()))?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        rows.push(row(
            id, "json", "default/no-recovery", &input, &outcome.output, elapsed,
            json!({"recoverability": format!("{:?}", outcome.recoverability), "stash_writes": outcome.stash_writes.len(),
                "numeric_literals_retained": id != "json-numeric-literals" || ["9007199254740993", "0.12345678901234567890123456789", "-0.0"].iter().all(|literal| outcome.output.contains(literal))}),
        )?);
    }

    let schema = crate::schema_canonical();
    let schema_input =
        serde_json::to_string(&schema).map_err(|error| DomainError::Json(error.to_string()))?;
    let start = Instant::now();
    let schema_output = SchemaCompressor::new().compress(&schema);
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let schema_wire = serde_json::to_string(&schema_output)
        .map_err(|error| DomainError::Json(error.to_string()))?;
    rows.push(row("schema-canonical", "schema", "default", &schema_input, &schema_wire, elapsed,
        json!({"output_is_object": schema_output.is_object(), "scope": "schema representation; descriptions may be shortened"}))?);

    let toon_input = json!(
        (0..100)
            .map(|index| json!({"id": index, "label": format!("value-{index}")}))
            .collect::<Vec<_>>()
    );
    let toon_wire =
        serde_json::to_string(&toon_input).map_err(|error| DomainError::Json(error.to_string()))?;
    let start = Instant::now();
    let toon_output = toon_format::encode_default(&toon_input)
        .map_err(|error| DomainError::Json(error.to_string()))?;
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    rows.push(row(
        "toon-rows",
        "toon",
        "representation-only",
        &toon_wire,
        &toon_output,
        elapsed,
        json!({"scope": "isolated TOON representation, not runtime acceptance"}),
    )?);

    for count in [3, 64, 256] {
        let search = crate::content_search_input(count);
        let start = Instant::now();
        let output = SearchResultsCompressor
            .compress(&search)
            .unwrap_or_else(|| search.clone());
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        let retained =
            (0..count).all(|index| output.contains(&format!("{index}:required_value_{index}\n")));
        rows.push(row(
            &format!("search-{count}"),
            "search-results",
            "direct component",
            &search,
            &output,
            elapsed,
            json!({"all_numbered_row_suffixes_retained": retained, "source_rows": count}),
        )?);

        let paths = crate::content_paths_input(count);
        let start = Instant::now();
        let output = SearchResultsCompressor
            .compress_paths(&paths)
            .unwrap_or_else(|| paths.clone());
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        rows.push(row(&format!("paths-{count}"), "path-list", "direct component", &paths, &output, elapsed,
            json!({"all_file_names_retained": (0..count).all(|index| output.contains(&format!("source_{index}.rs")))}))?);
    }

    for separator in [",", "\t"] {
        let input = crate::content_tabular_input(64, separator);
        let start = Instant::now();
        let outcome = TabularCompressor.compress(&input);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        let retained =
            (0..64).all(|index| outcome.output.contains(&format!("required_value_{index}")));
        rows.push(row(if separator == "," { "csv-64" } else { "tsv-64" }, "tabular", "lossless full-table", &input, &outcome.output, elapsed,
            json!({"all_messages_retained": retained, "original_rows": outcome.metrics.original_rows,
                "retained_rows": outcome.metrics.retained_rows, "stash_writes": outcome.stash_writes.len()}))?);
    }

    let log = crate::content_build_log_input(100);
    for recovery_available in [false, true] {
        let store = InMemoryStore::new();
        let stash = recovery_available.then_some(&store as &dyn StashStore);
        let start = Instant::now();
        let outcome = BuildLogCompressor.compress(&log, stash);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        let mut recovered_lines = 0usize;
        let mut recovered_blocks = 0usize;
        for write in &outcome.stash_writes {
            if let Some(payload) = store
                .retrieve(&write.key)
                .map_err(|error| DomainError::Recovery(error.to_string()))?
                && log.contains(&payload)
                && outcome.output.contains(&write.key)
            {
                recovered_lines += payload.lines().count();
                recovered_blocks += 1;
            }
        }
        let recovery_verified = recovered_blocks == outcome.metrics.omitted_blocks
            && recovered_lines == outcome.metrics.omitted_lines;
        rows.push(row(if recovery_available { "build-log-with-recovery" } else { "build-log-no-recovery" }, "build-log",
            if recovery_available { "recovery-available" } else { "default/no-recovery" }, &log, &outcome.output, elapsed,
            json!({"diagnostic_retained": outcome.output.contains("error[E0308]") && outcome.output.contains("expected bool, found String"),
                "recoverability": format!("{:?}", outcome.recoverability), "all_omitted_blocks_retrievable": recovery_verified,
                "recovered_blocks": recovered_blocks, "recovered_lines": recovered_lines,
                "omitted_blocks": outcome.metrics.omitted_blocks,
                "omitted_lines": outcome.metrics.omitted_lines, "stash_errors": outcome.metrics.stash_errors}))?);
    }

    let html = crate::content_html_input(100);
    let start = Instant::now();
    let view = HtmlExtractor.render(&html);
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let output = view
        .as_ref()
        .map_or(html.as_str(), |value| value.output.as_str());
    rows.push(row("html-page", "html", "direct renderer; runtime capability/recovery gates excluded", &html, output, elapsed,
        json!({"applicable": view.is_some(), "required_facts_retained": output.contains("required_heading") && output.contains("required_code = 42"),
            "scope": "rendered view is not byte-lossless; production use requires recovery"}))?);

    let short = "one unsupported search row";
    let start = Instant::now();
    let rejected = SearchResultsCompressor.compress(short);
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    rows.push(row(
        "search-negative-control",
        "search-results",
        "unsupported-control",
        short,
        rejected.as_deref().unwrap_or(short),
        elapsed,
        json!({"rejected": rejected.is_none()}),
    )?);

    let independent_payloads = rows
        .iter()
        .filter_map(|value| value["payload_hash"].as_str())
        .collect::<BTreeSet<_>>()
        .len();
    Ok(json!({
        "schema_version": 1,
        "headline_tokenizer": "o200k_base",
        "billing_tokens": "not measured by this offline tokenizer",
        "payload_hash_algorithm": "blake3",
        "measurement_scope": "isolated component candidates; runtime final arbitration excluded",
        "independent_payloads": independent_payloads,
        "rows": rows,
        "cost": Value::Null,
    }))
}
