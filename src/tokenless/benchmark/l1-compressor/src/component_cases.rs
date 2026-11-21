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

//! Deterministic wire inputs for Criterion and paired Headroom component tests.
//!
//! Each candidate records disposition, retention form, mechanism and fixture redundancy;
//! those describe an isolated component, not production Runtime acceptance.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Instant;
use tokenless_ccr::{InMemoryStore, StashStore, StashWrite};
use tokenless_compressors::{
    BuildLogCompressor, HtmlExtractor, JsonCompressionContext, JsonCompressor, Recoverability,
    RecoveryMethod, SearchResultsCompressor, TabularCompressor,
};
use tokenless_schema::SchemaCompressor;

use crate::domains::{DomainError, count_text};

/// Typed component failures, separating missing capability from rejected input.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ComponentError {
    /// No isolated component exists for this domain.
    #[error("no isolated component for domain {0}")]
    NoEntry(String),
    /// An existing component failed to process the supplied input.
    #[error("component rejected input: {0}")]
    Rejected(String),
}

fn has_component(domain: &str) -> bool {
    matches!(
        domain,
        "json" | "schema" | "search" | "paths" | "tabular" | "buildlog" | "html"
    )
}

/// Same-input component contract; facts score results and never become model hints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentCase {
    /// Stable identity for the fixture bytes.
    pub id: String,
    /// Isolated component domain, not an inferred Runtime route.
    pub domain: String,
    /// Explicit default, no-recovery or attached-store condition.
    pub mode: String,
    /// Original wire text supplied identically to both component sides.
    pub input: String,
    /// Required source facts whose removal must score as loss.
    pub facts: Vec<String>,
    /// Variable-form facts checked with Rust regex syntax, following L2's fact families.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,
    /// Match bound path/line/content records rather than independent substrings.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub search_records: bool,
    /// Source category/sample identity in L2; absent for synthetic fixtures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// Generate the fixed content and size matrix without launching tools or models.
pub fn cases() -> Vec<ComponentCase> {
    let mut output = Vec::new();
    for count in [64, 256, 1024] {
        let json = crate::content_json_input(count);
        let search = crate::content_search_input(count);
        let paths = crate::content_paths_input(count);
        let table = crate::content_tabular_input(count, ",");
        let log = crate::content_build_log_input(count);
        let html = crate::content_html_input(count);
        for (domain, input, facts) in [
            (
                "json",
                json,
                vec![
                    "required_value_0".into(),
                    format!("required_value_{}", count / 2),
                    format!("required_value_{}", count - 1),
                ],
            ),
            (
                "search",
                search,
                vec![
                    "required_value_0".into(),
                    format!("required_value_{}", count / 2),
                    format!("required_value_{}", count - 1),
                ],
            ),
            (
                "paths",
                paths,
                vec![
                    "source_0.rs".into(),
                    format!("source_{}.rs", count / 2),
                    format!("source_{}.rs", count - 1),
                ],
            ),
            (
                "tabular",
                table,
                vec![
                    "required_value_0".into(),
                    format!("required_value_{}", count / 2),
                    format!("required_value_{}", count - 1),
                ],
            ),
            (
                "buildlog",
                log,
                vec![
                    "required_diagnostic".into(),
                    "src/required_source.rs:12:4".into(),
                    "expected bool, found String".into(),
                ],
            ),
            (
                "html",
                html,
                vec![
                    "required_heading".into(),
                    "required_value_middle".into(),
                    "required_code = 42".into(),
                ],
            ),
        ] {
            let modes: &[&str] = if domain == "buildlog" {
                &["no-recovery", "recovery"]
            } else {
                &["direct-default"]
            };
            for mode in modes {
                output.push(ComponentCase {
                    id: format!("{domain}-{count}-{mode}"),
                    domain: domain.into(),
                    mode: (*mode).into(),
                    input: input.clone(),
                    facts: facts.clone(),
                    patterns: Vec::new(),
                    search_records: false,
                    origin: None,
                });
            }
        }
    }
    output.push(ComponentCase {
        id: "schema-canonical-direct".into(),
        domain: "schema".into(),
        mode: "direct-default".into(),
        input: serde_json::to_string(&crate::schema_canonical())
            .expect("controlled schema serializes"),
        facts: vec!["search_code".into(), "query".into(), "limit".into()],
        patterns: Vec::new(),
        search_records: false,
        origin: None,
    });
    output
}

/// Invoke the isolated component without production Runtime arbitration.
///
/// Timing-only: the temporary stash is dropped on return, so recovery markers
/// cannot certify retrievability. Use [`run`] for retained recovery evidence.
///
/// # Errors
/// Propagates parse/component failures without estimate or empty-output fallbacks.
pub fn compress(case: &ComponentCase) -> Result<String, ComponentError> {
    if !has_component(&case.domain) {
        return Err(ComponentError::NoEntry(case.domain.clone()));
    }
    compress_candidate(case).map_err(ComponentError::Rejected)
}

fn compress_candidate(case: &ComponentCase) -> Result<String, String> {
    match case.domain.as_str() {
        "json" => {
            let store = InMemoryStore::new();
            let recovery = json_recovery(case);
            JsonCompressor::default()
                .compress(
                    &case.input,
                    &JsonCompressionContext {
                        recovery: &recovery,
                        stash: recovery_store(case, &store),
                        allow_toon: false,
                        min_toon_chars: usize::MAX,
                        allow_unrecoverable: false,
                        embedded_html: false,
                    },
                )
                .map(|result| result.output)
                .map_err(|error| error.to_string())
        }
        "schema" => serde_json::from_str(&case.input)
            .map_err(|error| error.to_string())
            .and_then(|value| {
                serde_json::to_string(&SchemaCompressor::new().compress(&value))
                    .map_err(|error| error.to_string())
            }),
        "search" => Ok(SearchResultsCompressor
            .compress(&case.input)
            .unwrap_or_else(|| case.input.clone())),
        "paths" => Ok(SearchResultsCompressor
            .compress_paths(&case.input)
            .unwrap_or_else(|| case.input.clone())),
        "tabular" => Ok(TabularCompressor.compress(&case.input).output),
        "buildlog" => {
            let store = InMemoryStore::new();
            Ok(BuildLogCompressor
                .compress(&case.input, recovery_store(case, &store))
                .output)
        }
        "html" => Ok(HtmlExtractor
            .render(&case.input)
            .map_or_else(|| case.input.clone(), |view| view.output)),
        domain => Err(format!("unknown isolated component domain: {domain}")),
    }
}

/// Modes in which the component is handed a recovery store.
fn has_recovery(case: &ComponentCase) -> bool {
    case.mode == "recovery"
}

fn recovery_store<'a>(
    case: &ComponentCase,
    store: &'a InMemoryStore,
) -> Option<&'a dyn StashStore> {
    has_recovery(case).then_some(store as &dyn StashStore)
}

fn json_recovery(case: &ComponentCase) -> RecoveryMethod {
    if has_recovery(case) {
        RecoveryMethod::Shell
    } else {
        RecoveryMethod::None
    }
}

/// One Stash reference the candidate carries and the bytes a retrieve returns.
#[derive(Debug, Clone, Serialize)]
pub struct StashEntry {
    /// Recovery reference visible in the candidate output.
    pub key: String,
    /// Exact original bytes read back for that reference.
    pub original: String,
}

/// One component candidate with the facts that explain its rate.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    /// Candidate text the component produced (the input when it declined).
    pub output: String,
    /// The component's own recovery claim: `lossless`, `retrievable` or
    /// `unrecoverable`.
    pub recoverability: &'static str,
    /// Which operator ran and what it removed.
    pub mechanism: String,
    /// Stash entries the candidate references, read back from the store, so a
    /// retrieval-enabled semantic test can serve them.
    pub stash: Vec<StashEntry>,
}

/// Originals behind every write the candidate actually references.
///
/// # Errors
/// A referenced key the store cannot return: an emitted marker without its
/// bytes is an unrecoverable deletion and must not pass as recoverable.
fn referenced_stash(
    store: &InMemoryStore,
    writes: &[StashWrite],
    output: &str,
) -> Result<Vec<StashEntry>, String> {
    writes
        .iter()
        .filter(|write| output.contains(&write.key))
        .map(|write| {
            store
                .retrieve(&write.key)
                .map_err(|error| error.to_string())?
                .map(|original| StashEntry {
                    key: write.key.clone(),
                    original,
                })
                .ok_or_else(|| format!("emitted stash reference {} is not retrievable", write.key))
        })
        .collect()
}

fn recoverability_name(value: Recoverability) -> &'static str {
    match value {
        Recoverability::Lossless => "lossless",
        Recoverability::Retrievable => "retrievable",
        Recoverability::Unrecoverable => "unrecoverable",
    }
}

/// Runs the component like [`compress`] and records how the candidate came about.
///
/// # Errors
/// Same failures as [`compress`]; a domain without an isolated component is an
/// error here and becomes the `no-entry` disposition in [`measure`].
pub fn run(case: &ComponentCase) -> Result<Candidate, ComponentError> {
    if !has_component(&case.domain) {
        return Err(ComponentError::NoEntry(case.domain.clone()));
    }
    run_candidate(case).map_err(ComponentError::Rejected)
}

fn run_candidate(case: &ComponentCase) -> Result<Candidate, String> {
    let declined = |mechanism: &str| Candidate {
        output: case.input.clone(),
        recoverability: "lossless",
        mechanism: mechanism.to_owned(),
        stash: Vec::new(),
    };
    match case.domain.as_str() {
        "json" => {
            let store = InMemoryStore::new();
            let recovery = json_recovery(case);
            let outcome = JsonCompressor::default()
                .compress(
                    &case.input,
                    &JsonCompressionContext {
                        recovery: &recovery,
                        stash: recovery_store(case, &store),
                        allow_toon: false,
                        min_toon_chars: usize::MAX,
                        allow_unrecoverable: false,
                        embedded_html: false,
                    },
                )
                .map_err(|error| error.to_string())?;
            let operations = outcome
                .operations
                .iter()
                .map(|operation| operation.id())
                .collect::<Vec<_>>();
            Ok(Candidate {
                mechanism: format!(
                    "JsonCompressor [{}]: {} records omitted in {} reductions, {} truncations",
                    operations.join("+"),
                    outcome.metrics.records_omitted,
                    outcome.metrics.record_reductions,
                    outcome.metrics.truncations
                ),
                recoverability: recoverability_name(outcome.recoverability),
                stash: referenced_stash(&store, &outcome.stash_writes, &outcome.output)?,
                output: outcome.output,
            })
        }
        "schema" => {
            let value: Value =
                serde_json::from_str(&case.input).map_err(|error| error.to_string())?;
            let output = serde_json::to_string(&SchemaCompressor::new().compress(&value))
                .map_err(|error| error.to_string())?;
            Ok(Candidate {
                output,
                recoverability: "unrecoverable",
                mechanism:
                    "SchemaCompressor: descriptions, titles and examples shortened or dropped"
                        .into(),
                stash: Vec::new(),
            })
        }
        "search" => Ok(match SearchResultsCompressor.compress(&case.input) {
            Some(output) => Candidate {
                output,
                recoverability: "lossless",
                mechanism:
                    "SearchResultsCompressor: consecutive identical paths share one File= header"
                        .into(),
                stash: Vec::new(),
            },
            None => {
                declined("SearchResultsCompressor declined: not a numbered listing or no saving")
            }
        }),
        "paths" => Ok(match SearchResultsCompressor.compress_paths(&case.input) {
            Some(output) => Candidate {
                output,
                recoverability: "lossless",
                mechanism:
                    "SearchResultsCompressor::compress_paths: common directory prefix shared".into(),
                stash: Vec::new(),
            },
            None => declined("compress_paths declined: not a path list or no saving"),
        }),
        "tabular" => {
            let outcome = TabularCompressor.compress(&case.input);
            Ok(Candidate {
                mechanism: format!(
                    "TabularCompressor [{}]: {}/{} rows retained; quoting and CRLF compacted",
                    outcome
                        .operations
                        .iter()
                        .map(|operation| format!("{operation:?}"))
                        .collect::<Vec<_>>()
                        .join("+"),
                    outcome.metrics.retained_rows,
                    outcome.metrics.original_rows
                ),
                recoverability: recoverability_name(outcome.recoverability),
                stash: Vec::new(),
                output: outcome.output,
            })
        }
        "buildlog" => {
            let store = InMemoryStore::new();
            let outcome = BuildLogCompressor.compress(&case.input, recovery_store(case, &store));
            Ok(Candidate {
                mechanism: format!(
                    "BuildLogCompressor [{}]: {} routine lines folded into {} blocks",
                    outcome
                        .operations
                        .iter()
                        .map(|operation| operation.id())
                        .collect::<Vec<_>>()
                        .join("+"),
                    outcome.metrics.omitted_lines,
                    outcome.metrics.omitted_blocks
                ),
                recoverability: recoverability_name(outcome.recoverability),
                stash: referenced_stash(&store, &outcome.stash_writes, &outcome.output)?,
                output: outcome.output,
            })
        }
        "html" => Ok(match HtmlExtractor.render(&case.input) {
            // The isolated renderer has no store: whatever it drops is gone
            // unless the Runtime attaches the original behind a marker.
            Some(view) => Candidate {
                mechanism: format!(
                    "HtmlExtractor: rendered <{}> as Markdown; {} non-content elements removed, {} nodes outside the root",
                    view.root,
                    view.removed.iter().sum::<usize>(),
                    view.outside_root
                ),
                recoverability: "unrecoverable",
                stash: Vec::new(),
                output: view.output,
            },
            None => declined("HtmlExtractor declined: rendered body too short or nesting too deep"),
        }),
        domain => Err(format!("no isolated component for domain {domain}")),
    }
}

/// Share of the input a component of this domain targets, and what that is.
///
/// Explains a synthetic rate: an input that is 90% CSS yields a 90% candidate
/// whatever the renderer's quality, and that is a property of the fixture.
pub fn redundancy(case: &ComponentCase) -> Option<Value> {
    let total = case.input.len();
    if total == 0 {
        return None;
    }
    let (kind, bytes) = match case.domain.as_str() {
        "json" => (
            "pretty-print whitespace outside strings",
            json_layout_bytes(&case.input),
        ),
        "search" => {
            let mut previous: Option<&str> = None;
            let mut repeated = 0;
            for line in case.input.lines() {
                let path = line.split(':').next().unwrap_or_default();
                if previous == Some(path) {
                    repeated += path.len() + 1;
                }
                previous = Some(path);
            }
            ("repeated path prefixes", repeated)
        }
        "paths" => {
            let lines: Vec<&str> = case.input.lines().collect();
            let prefix = common_directory_prefix(&lines);
            (
                "repeated common directory prefix",
                prefix * lines.len().saturating_sub(1),
            )
        }
        "tabular" => (
            "quotes and CR terminators",
            case.input
                .bytes()
                .filter(|byte| matches!(byte, b'"' | b'\r'))
                .count(),
        ),
        "buildlog" => (
            "`Compiling` progress lines",
            case.input
                .split_inclusive('\n')
                .filter(|line| line.trim_start().starts_with("Compiling "))
                .map(str::len)
                .sum(),
        ),
        "html" => ("<style> CSS bytes", html_style_bytes(&case.input)),
        "schema" => {
            let Ok(value) = serde_json::from_str::<Value>(&case.input) else {
                return None;
            };
            ("description/title/example text", descriptive_bytes(&value))
        }
        _ => return None,
    };
    Some(json!({
        "kind": kind,
        "bytes": bytes,
        "share_of_input_bytes": bytes as f64 / total as f64,
    }))
}

fn json_layout_bytes(input: &str) -> usize {
    let mut in_string = false;
    let mut escaped = false;
    let mut count = 0;
    for byte in input.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte.is_ascii_whitespace() {
            count += 1;
        }
    }
    count
}

fn common_directory_prefix(lines: &[&str]) -> usize {
    let Some(first) = lines.first() else {
        return 0;
    };
    let mut prefix = first.rfind('/').map_or(0, |index| index + 1);
    for line in &lines[1..] {
        while prefix > 0 && !line.starts_with(&first[..prefix]) {
            prefix = first[..prefix - 1].rfind('/').map_or(0, |index| index + 1);
        }
    }
    prefix
}

fn html_style_bytes(input: &str) -> usize {
    let lower = input.to_ascii_lowercase();
    let mut total = 0;
    let mut rest = 0;
    while let Some(start) = lower[rest..].find("<style") {
        let begin = rest + start;
        let Some(end) = lower[begin..].find("</style>") else {
            break;
        };
        total += end + "</style>".len();
        rest = begin + end + "</style>".len();
    }
    total
}

fn descriptive_bytes(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, child)| match (key.as_str(), child) {
                ("description" | "title", Value::String(text)) => text.len(),
                ("examples", examples) => examples.to_string().len(),
                _ => descriptive_bytes(child),
            })
            .sum(),
        Value::Array(items) => items.iter().map(descriptive_bytes).sum(),
        _ => 0,
    }
}

/// Outcome of the fact checks on one candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FactScore {
    /// Source facts still present in the candidate.
    pub passed: usize,
    /// Actual checked denominator; zero means unmeasured retention.
    pub total: usize,
    /// Required source facts that could not be found.
    pub missing: Vec<String>,
}

/// Checks a case's facts against a candidate under the L2 rules: substrings
/// verbatim or after [`normalize_format`], patterns as regexes, and search
/// records as path/line/content triples after [`normalized_search_records`].
///
/// # Errors
/// An invalid pattern is an asset bug and surfaces instead of scoring as a miss.
pub fn check_facts(case: &ComponentCase, output: &str) -> Result<FactScore, regex::Error> {
    let mut score = FactScore {
        total: case.facts.len() + case.patterns.len(),
        ..FactScore::default()
    };
    if case.search_records {
        let normalized = normalized_search_records(output);
        let records: std::collections::HashSet<String> =
            normalized.lines().map(normalize_format).collect();
        for fact in &case.facts {
            if records.contains(&normalize_format(fact)) {
                score.passed += 1;
            } else {
                score.missing.push(fact.clone());
            }
        }
        for pattern in &case.patterns {
            if Regex::new(pattern)?.is_match(&normalized) {
                score.passed += 1;
            } else {
                score.missing.push(format!("/{pattern}/"));
            }
        }
        return Ok(score);
    }
    let mut normalized: Option<String> = None;
    for fact in &case.facts {
        let found = output.contains(fact.as_str())
            || normalized
                .get_or_insert_with(|| normalize_format(output))
                .contains(normalize_format(fact).as_str());
        if found {
            score.passed += 1;
        } else {
            score.missing.push(fact.clone());
        }
    }
    for pattern in &case.patterns {
        if Regex::new(pattern)?.is_match(output) {
            score.passed += 1;
        } else {
            score.missing.push(format!("/{pattern}/"));
        }
    }
    Ok(score)
}

/// Format-only normalization for fact matching, identical to L2's: Markdown
/// backslash escapes are removed and whitespace runs collapse to one space.
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

/// Restores path/line/body bindings for fact checks only, identical to L2's:
/// raw grep lines, tokenless `File=` headers and full-path headers.
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

/// Disposition of a component candidate.
///
/// `no-entry` (no isolated component) and `candidate-rejected` (the component
/// failed on the input) are kept apart from `passthrough` (it declined or
/// returned the input) so a 0% row says why it is 0%.
pub fn disposition(
    result: &Result<Candidate, ComponentError>,
    input: &str,
    before: usize,
    after: Option<usize>,
) -> &'static str {
    match result {
        Err(ComponentError::NoEntry(_)) => "no-entry",
        Err(_) => "candidate-rejected",
        Ok(candidate) if candidate.output == input => "passthrough",
        Ok(_) if after.is_some_and(|after| after > before) => "expanded",
        Ok(_) => "compressed",
    }
}

/// Retention form of a candidate: inline, offloaded behind a recovery marker,
/// or deleted without a recovery path.
pub fn retention_form(result: &Result<Candidate, ComponentError>, input: &str) -> &'static str {
    match result {
        Ok(candidate) if candidate.output != input => match candidate.recoverability {
            "retrievable" => "offloaded-with-marker",
            "unrecoverable" => "unrecoverable-deletion",
            _ => "inline",
        },
        _ => "inline",
    }
}

/// Measures one case: the candidate, its disposition and retention form,
/// the fact checks and the redundancy share, on the `o200k_base` scale.
///
/// # Errors
/// Tokenizer failures and invalid fact patterns.
pub fn measure(case: &ComponentCase) -> Result<Value, DomainError> {
    let start = Instant::now();
    let result = run(case);
    let elapsed = start.elapsed().as_secs_f64();
    let before = count_text(&case.input)?;
    let after = result
        .as_ref()
        .ok()
        .map(|candidate| count_text(&candidate.output))
        .transpose()?;
    let facts = match &result {
        Ok(candidate) => Some(
            check_facts(case, &candidate.output)
                .map_err(|error| DomainError::Json(format!("{}: {error}", case.id)))?,
        ),
        Err(_) => None,
    };
    let original_facts = check_facts(case, &case.input)
        .map_err(|error| DomainError::Json(format!("{}: {error}", case.id)))?;
    if original_facts.passed != original_facts.total {
        return Err(DomainError::InvalidFacts(format!(
            "{}: original does not satisfy its own fact contract",
            case.id
        )));
    }
    Ok(json!({
        "case": case,
        "output": result.as_ref().ok().map(|candidate| &candidate.output),
        "error": result.as_ref().err().map(|error| error.to_string()),
        "error_kind": result.as_ref().err().map(|error| match error { ComponentError::NoEntry(_) => "no-entry", ComponentError::Rejected(_) => "rejected" }),
        "tokens_before": before,
        "tokens_after": after,
        "compression_rate": after.filter(|_| before > 0).map(|after| 1.0 - after as f64 / before as f64),
        "disposition": disposition(&result, &case.input, before, after),
        "retention_form": retention_form(&result, &case.input),
        "mechanism": result.as_ref().map(|candidate| candidate.mechanism.clone()).unwrap_or_else(|error| error.to_string()),
        "stash_entries": result.as_ref().map(|candidate| candidate.stash.len()).unwrap_or(0),
        // Retrieval fixtures serve these verified originals without calling Runtime again.
        "stash": result.as_ref().map(|candidate| candidate.stash.clone()).unwrap_or_default(),
        "redundancy": redundancy(case),
        "facts": facts,
        "original_facts": original_facts,
        "latency_s": elapsed,
        "latency_basis": "isolated wire component call; not Criterion estimate",
        "headline_tokenizer": "o200k_base",
        "scope": "component candidate; runtime final arbitration excluded",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_have_nonempty_truth_and_reject_content_free_controls() {
        let cases = cases();
        assert_eq!(cases.len(), 22);
        for case in cases {
            assert!(!case.input.is_empty());
            assert!(!case.facts.is_empty());
            for fact in &case.facts {
                assert!(
                    case.input.contains(fact),
                    "{} has invalid original truth: {fact}",
                    case.id
                );
                assert!(!"empty-format-frame".contains(fact));
            }
            let compressed = compress(&case).expect("component contract runs");
            for fact in &case.facts {
                assert!(compressed.contains(fact), "{} lost {fact}", case.id);
            }
        }
    }

    #[test]
    fn run_matches_compress_and_explains_every_row() {
        for case in cases() {
            let candidate = run(&case).expect("component contract runs");
            assert_eq!(candidate.output, compress(&case).unwrap(), "{}", case.id);
            assert!(!candidate.mechanism.is_empty());
            let row = measure(&case).unwrap();
            assert_eq!(row["facts"]["passed"], row["facts"]["total"], "{}", case.id);
            assert_eq!(
                row["original_facts"]["passed"],
                row["original_facts"]["total"]
            );
            let share = row["redundancy"]["share_of_input_bytes"].as_f64().unwrap();
            assert!(share > 0.0 && share < 1.0, "{} redundancy {share}", case.id);
            assert!(
                ["compressed", "passthrough", "expanded"]
                    .contains(&row["disposition"].as_str().unwrap())
            );
        }
    }

    #[test]
    fn recovery_mode_offloads_and_no_recovery_does_not() {
        let all = cases();
        let with = all
            .iter()
            .find(|case| case.id == "buildlog-1024-recovery")
            .unwrap();
        let without = all
            .iter()
            .find(|case| case.id == "buildlog-1024-no-recovery")
            .unwrap();
        let with = measure(with).unwrap();
        let without = measure(without).unwrap();
        assert_eq!(with["retention_form"], "offloaded-with-marker");
        assert!(with["stash_entries"].as_u64().unwrap() > 0);
        let output = with["output"].as_str().unwrap();
        let input = with["case"]["input"].as_str().unwrap();
        for entry in with["stash"].as_array().unwrap() {
            assert!(output.contains(entry["key"].as_str().unwrap()));
            assert!(input.contains(entry["original"].as_str().unwrap()));
        }
        assert_eq!(without["stash_entries"], 0);
        assert_ne!(without["retention_form"], "offloaded-with-marker");
    }

    #[test]
    fn original_fact_failures_are_invalid_evidence_not_product_scores() {
        let mut case = cases().remove(0);
        case.facts
            .push("fact_that_never_existed_in_the_input".into());
        assert!(matches!(measure(&case), Err(DomainError::InvalidFacts(_))));
        let rejected = Err(ComponentError::Rejected(
            "no isolated component is just error text here".into(),
        ));
        assert_eq!(
            disposition(&rejected, "input", 1, None),
            "candidate-rejected"
        );
    }

    #[test]
    fn missing_component_and_failures_are_not_passthrough() {
        let mut case = cases().remove(0);
        case.domain = "diff".into();
        let result = run(&case);
        assert_eq!(disposition(&result, &case.input, 10, None), "no-entry");
        case.domain = "json".into();
        case.input = "{not json".into();
        let result = run(&case);
        assert_eq!(
            disposition(&result, &case.input, 10, None),
            "candidate-rejected"
        );
    }

    #[test]
    fn fact_checks_normalize_format_but_report_removed_content() {
        let case = ComponentCase {
            id: "t".into(),
            domain: "html".into(),
            mode: "direct-default".into(),
            input: "required_heading and complete explanatory evidence".into(),
            facts: vec![
                "required_heading".into(),
                "complete explanatory evidence".into(),
            ],
            patterns: vec![r"E\d{4}".into()],
            search_records: false,
            origin: None,
        };
        let escaped = "# required\\_heading\ncomplete\n  explanatory evidence E0308";
        assert_eq!(check_facts(&case, escaped).unwrap().passed, 3);
        // Reverse control: an empty frame and a near-miss must both fail.
        let frame = check_facts(&case, "# required\\_section\nexplanatory").unwrap();
        assert_eq!(frame.passed, 0);
        assert_eq!(frame.missing.len(), 3);
    }

    #[test]
    fn search_records_bind_paths_from_shared_headers() {
        let case = ComponentCase {
            id: "grep".into(),
            domain: "search".into(),
            mode: "direct-default".into(),
            input: "src/a.rs:3:fn main() {}\nsrc/a.rs:9:fn other() {}\n".into(),
            facts: vec![
                "src/a.rs:3:fn main() {}".into(),
                "src/a.rs:9:fn other() {}".into(),
            ],
            patterns: Vec::new(),
            search_records: true,
            origin: None,
        };
        let shared = "[Search results grouped by file; all received lines retained]\nFile=\"src/a.rs\"\n3:fn main() {}\n9:fn other() {}\n";
        assert_eq!(check_facts(&case, shared).unwrap().passed, 2);
        let dropped = "File=\"src/a.rs\"\n3:fn main() {}\n";
        assert_eq!(check_facts(&case, dropped).unwrap().passed, 1);
    }
}
