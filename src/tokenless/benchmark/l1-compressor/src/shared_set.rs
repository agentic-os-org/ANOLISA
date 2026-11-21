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

//! Pair isolated candidates with the production outcomes in caller-supplied L2 rows.
//!
//! Repeated category/sample/payload tuples count once. Domains and fact rules come
//! from the same L2 evidence, so candidate/Runtime differences remain explainable.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokenless_compressors::SearchResultsCompressor;

use crate::component_cases::{ComponentCase, measure};
use crate::domains::DomainError;

/// What the production entry did to the same payload in L2.
#[derive(Debug, Clone, Serialize)]
pub struct L2Final {
    /// L2 input category used to join source evidence.
    pub category: String,
    /// Source sample identity, before aliases are collapsed.
    pub sample_id: String,
    /// Actual selected Runtime condition.
    pub mode: String,
    /// Fingerprint of the original measured bytes.
    pub payload_hash: String,
    /// Native, rewritten or explicit RTK entry condition.
    pub entry_kind: String,
    /// Runtime's reported origin, never inferred from content alone.
    pub content_origin: Option<String>,
    /// Tool success/error state recorded by the host.
    pub tool_status: Option<String>,
    /// Detected PostTool content domain.
    pub content_type: Option<String>,
    /// Production arbitration result.
    pub disposition: Option<String>,
    /// Recovery guarantee reported by Runtime.
    pub recoverability: Option<String>,
    /// Wire names of the operations Runtime selected.
    pub operations: Vec<String>,
    /// Referenced originals exported by the source row.
    pub stash_entries: usize,
    /// Original o200k_base count in the source evidence.
    pub tokens_before: u64,
    /// Final model-visible o200k_base count.
    pub tokens_after: u64,
    /// Source rate; not a newly measured candidate rate.
    pub compression_rate: Option<f64>,
    /// Source facts still present after production processing.
    pub retention_passed: u64,
    /// Actual source fact-check denominator.
    pub retention_total: u64,
}

/// One L2 payload as a component case, paired with its L2 final state.
#[derive(Debug, Clone)]
pub struct SharedCase {
    /// Isolated candidate evaluated on the source input and facts.
    pub case: ComponentCase,
    /// Production result against which the candidate is compared.
    pub l2: L2Final,
}

/// Loads the tokenless rows of `mode` from an L2 `measurements.json`.
///
/// # Errors
/// An unreadable or malformed file, or a row missing the fields every L2
/// tokenless row carries.
pub fn load(path: &Path, mode: &str) -> Result<Vec<SharedCase>, DomainError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| DomainError::Json(format!("cannot read {}: {error}", path.display())))?;
    let doc: Value = serde_json::from_str(&text)
        .map_err(|error| DomainError::Json(format!("{}: {error}", path.display())))?;
    from_measurements(&doc, mode)
}

/// [`load`] on an already-parsed document.
///
/// # Errors
/// A tokenless row missing `sample_id`, `input` or the token counts.
pub fn from_measurements(doc: &Value, mode: &str) -> Result<Vec<SharedCase>, DomainError> {
    let rows = doc["rows"]
        .as_array()
        .ok_or_else(|| DomainError::Json("measurements have no rows array".into()))?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in rows {
        if entry["side"] != "tokenless" || entry["mode"] != mode {
            continue;
        }
        let row = &entry["row"];
        let category = entry["category"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| DomainError::Json("L2 row lacks category identity".into()))?
            .to_owned();
        let field = |name: &str| {
            row[name]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| DomainError::Json(format!("L2 row lacks {name}: {category}")))
        };
        let sample_id = field("sample_id")?;
        let payload_hash = field("payload_hash")?;
        if !seen.insert((category.clone(), sample_id.clone(), payload_hash.clone())) {
            continue;
        }
        let input = if let Some(reference) = row["input_ref"].as_str() {
            let input = doc["text_store"][reference]
                .as_str()
                .ok_or_else(|| DomainError::Json("L2 input_ref has no text_store entry".into()))?;
            if format!("{:x}", Sha256::digest(input.as_bytes())) != reference
                || payload_hash != reference
            {
                return Err(DomainError::Json(
                    "L2 input_ref does not match the input fingerprint".into(),
                ));
            }
            input.to_owned()
        } else {
            field("input")?
        };
        let tokens_before = row["tokens_before"]
            .as_u64()
            .ok_or_else(|| DomainError::Json("L2 row lacks tokens_before".into()))?;
        if tokens_before == 0 {
            // Empty-input controls carry no efficacy information in L2 either.
            continue;
        }
        let post = &row["entry"]["post_tool"];
        let text = |value: &Value| value.as_str().map(str::to_owned);
        let content_type = text(&post["content_type"]);
        let mut facts = Vec::new();
        let mut patterns = Vec::new();
        for truth in row["ground_truth"].as_array().into_iter().flatten() {
            match truth {
                Value::String(fact) => facts.push(fact.clone()),
                Value::Object(map) => {
                    if let Some(regex) = map.get("regex").and_then(Value::as_str) {
                        patterns.push(regex.to_owned());
                    }
                }
                _ => {}
            }
        }
        let domain = component_domain(content_type.as_deref(), &input)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                content_type
                    .clone()
                    .unwrap_or_else(|| "unclassified".into())
            });
        // The L2 default and diff-opt-in modes attach a store; no-stash does not.
        let component_mode = if mode == "no-stash" {
            "no-recovery"
        } else {
            "recovery"
        };
        let tokens_after = row["tokens_after"]
            .as_u64()
            .ok_or_else(|| DomainError::Json("L2 row lacks tokens_after".into()))?;
        out.push(SharedCase {
            case: ComponentCase {
                id: format!("l2-{category}-{sample_id}"),
                domain,
                mode: component_mode.into(),
                input,
                facts,
                patterns,
                search_records: category == "grep",
                origin: Some(format!("{category}/{sample_id}")),
            },
            l2: L2Final {
                category,
                sample_id,
                mode: mode.to_owned(),
                payload_hash,
                entry_kind: entry["entry_kind"].as_str().unwrap_or_default().to_owned(),
                content_origin: text(&row["entry"]["content_origin"]),
                tool_status: text(&row["entry"]["tool_status"]),
                content_type,
                disposition: text(&post["disposition"]),
                recoverability: text(&post["recoverability"]),
                operations: post["applied_operations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect(),
                stash_entries: row["entry"]["stash"].as_array().map_or(0, Vec::len),
                tokens_before,
                tokens_after,
                compression_rate: row["compression_rate"].as_f64(),
                retention_passed: row["retention_passed"].as_u64().unwrap_or(0),
                retention_total: row["retention_total"].as_u64().unwrap_or(0),
            },
        });
    }
    Ok(out)
}

/// The isolated component for a Runtime-detected content domain, if one exists.
///
/// Diff reduction, source code and prose have no isolated component: diff
/// reduction is a Runtime pipeline stage, and file content and prose are never
/// compressed, so those rows are `no-entry` rather than 0%.
pub fn component_domain(content_type: Option<&str>, input: &str) -> Option<&'static str> {
    match content_type? {
        "json" => Some("json"),
        "search_results" if SearchResultsCompressor.is_path_list(input) => Some("paths"),
        "search_results" => Some("search"),
        "build_log" => Some("buildlog"),
        "tabular" => Some("tabular"),
        "html" => Some("html"),
        _ => None,
    }
}

/// Measures every shared case and explains its distance to the L2 final state.
///
/// # Errors
/// Tokenizer failures and invalid fact patterns.
pub fn measure_all(cases: &[SharedCase]) -> Result<Vec<Value>, DomainError> {
    cases
        .iter()
        .map(|shared| {
            let mut row = match measure(&shared.case) {
                Ok(row) => row,
                Err(error @ DomainError::Tokenizer(_)) => return Err(error),
                Err(error) => json!({
                    "case": shared.case, "error": error.to_string(), "error_kind": "invalid-evidence",
                    "disposition": "invalid-evidence", "output": Value::Null,
                    "tokens_before": Value::Null, "tokens_after": Value::Null,
                    "facts": Value::Null, "original_facts": Value::Null,
                }),
            };
            let explanation = gap(&row, &shared.l2, &shared.case.input);
            row["l2_final"] = json!(shared.l2);
            row["gap"] = json!(explanation);
            Ok(row)
        })
        .collect()
}

/// `MIN_RESPONSE_CHARS` in `tokenless-runtime/src/entry.rs`: the production
/// entry passes shorter results through before any domain compressor runs.
pub const RUNTIME_MIN_RESPONSE_CHARS: usize = 200;

/// Why the Runtime returned a payload unchanged, with the gate that decided it.
pub fn passthrough_reason(l2: &L2Final, input: &str) -> String {
    if l2.content_origin.as_deref() == Some("file_content") {
        return "authoritative file content is never replaced \
                (PostToolPipeline::run, tokenless-runtime/src/post_tool/pipeline.rs)"
            .into();
    }
    let chars = input.chars().count();
    if chars < RUNTIME_MIN_RESPONSE_CHARS {
        return format!(
            "{chars} chars is below the production entry's MIN_RESPONSE_CHARS={RUNTIME_MIN_RESPONSE_CHARS} \
             gate (tokenless-runtime/src/entry.rs)"
        );
    }
    format!(
        "no candidate gate admits {} from {} \
         (PostToolPipeline::run, tokenless-runtime/src/post_tool/pipeline.rs)",
        l2.content_type.as_deref().unwrap_or("unclassified content"),
        l2.content_origin.as_deref().unwrap_or("this origin")
    )
}

/// Why the component candidate and the L2 final state differ, in one line.
pub fn gap(row: &Value, l2: &L2Final, input: &str) -> String {
    let candidate = row["disposition"].as_str().unwrap_or_default();
    let final_disposition = l2.disposition.as_deref().unwrap_or("unknown");
    let operations = if l2.operations.is_empty() {
        String::new()
    } else {
        format!(" [{}]", l2.operations.join("+"))
    };
    let candidate_after = row["tokens_after"].as_u64();
    match (candidate, final_disposition) {
        ("invalid-evidence", _) => {
            "the original fact contract is invalid; candidate/Runtime comparison is not publishable"
                .into()
        }
        ("no-entry", "passthrough") => format!(
            "no isolated component for {}; Runtime passthrough: {}",
            l2.content_type.as_deref().unwrap_or("unclassified content"),
            passthrough_reason(l2, input)
        ),
        ("no-entry", _) => format!(
            "no isolated component for {}; Runtime final: {final_disposition}{operations}",
            l2.content_type.as_deref().unwrap_or("unclassified content")
        ),
        (_, "tool_error") => format!(
            "Runtime never replaces an error result (status {}); the {candidate} candidate is unused",
            l2.tool_status.as_deref().unwrap_or("error")
        ),
        ("compressed" | "expanded", "applied") => match candidate_after {
            Some(after) if after == l2.tokens_after => {
                "identical to the Runtime final state".into()
            }
            Some(after) => format!(
                "Runtime final {} tokens vs candidate {after}{operations}: Runtime arbitration, \
                 TOON or recovery markers differ from the isolated component",
                l2.tokens_after
            ),
            None => "candidate unavailable".into(),
        },
        (_, "passthrough") => format!(
            "Runtime passthrough before arbitration: {}",
            passthrough_reason(l2, input)
        ),
        ("compressed" | "expanded", "no_savings") => {
            "Runtime savings gate rejected the candidate: it did not save both characters and \
             tokens (Disposition::NoSavings)"
                .into()
        }
        ("compressed" | "expanded", "recoverability_unavailable") => {
            "lossy candidate rejected: no marker-authorized recovery path in this mode \
             (Disposition::RecoverabilityUnavailable)"
                .into()
        }
        ("compressed" | "expanded", _) => format!(
            "Runtime rejected the candidate: {final_disposition} (recoverability {})",
            l2.recoverability.as_deref().unwrap_or("unknown")
        ),
        ("passthrough", "applied") => format!(
            "Runtime applied{operations} beyond this component (e.g. TOON or envelope unwrapping)"
        ),
        ("candidate-rejected", _) => format!(
            "the component failed on the input; Runtime final: {final_disposition}{operations}"
        ),
        _ => format!("neither changed the payload; Runtime final: {final_disposition}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measurements() -> Value {
        let log = format!(
            "{}error[E0308]: mismatched types\n --> src/main.rs:5:13\n",
            (0..40)
                .map(|index| format!("   Compiling dep{index:02} v0.1.0\n"))
                .collect::<String>()
        );
        json!({"rows": [
            {"category": "buildlog", "side": "tokenless", "mode": "default", "entry_kind": "production PostTool",
             "row": {"sample_id": "log", "rep": 0, "payload_hash": "h1", "input": log,
                     "ground_truth": ["error[E0308]: mismatched types", {"regex": "src/main\\.rs:\\d+"}],
                     "tokens_before": 300, "tokens_after": 120, "compression_rate": 0.6,
                     "retention_passed": 2, "retention_total": 2,
                     "entry": {"content_origin": "command_output", "tool_status": "success",
                               "post_tool": {"disposition": "applied", "content_type": "build_log",
                                             "recoverability": "retrievable",
                                             "applied_operations": ["build_log_reduction"]},
                               "stash": [{"key": "k", "original": "x"}]}}},
            {"category": "buildlog", "side": "tokenless", "mode": "default", "entry_kind": "production PostTool",
             "row": {"sample_id": "log", "rep": 1, "payload_hash": "h1", "input": "duplicate rep",
                     "ground_truth": [], "tokens_before": 300, "tokens_after": 120}},
            {"category": "code", "side": "tokenless", "mode": "default", "entry_kind": "production PostTool",
             "row": {"sample_id": "src", "rep": 0, "payload_hash": "h2", "input": "fn main() {}\n",
                     "ground_truth": ["fn main"], "tokens_before": 5, "tokens_after": 5,
                     "entry": {"content_origin": "file_content", "tool_status": "success",
                               "post_tool": {"disposition": "passthrough", "content_type": "source_code",
                                             "recoverability": "lossless", "applied_operations": []},
                               "stash": []}}},
            {"category": "json", "side": "headroom", "mode": "headroom ContentRouter worker",
             "row": {"sample_id": "j", "payload_hash": "h3", "input": "[]", "tokens_before": 1}},
            {"category": "json", "side": "tokenless", "mode": "no-stash",
             "row": {"sample_id": "j", "payload_hash": "h3", "input": "[]", "tokens_before": 1}}
        ]})
    }

    #[test]
    fn loads_one_case_per_distinct_tokenless_payload_of_the_mode() {
        let cases = from_measurements(&measurements(), "default").unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].case.domain, "buildlog");
        assert_eq!(cases[0].case.mode, "recovery");
        assert_eq!(
            cases[0].case.facts,
            vec!["error[E0308]: mismatched types".to_owned()]
        );
        assert_eq!(cases[0].case.patterns.len(), 1);
        assert_eq!(cases[0].l2.stash_entries, 1);
        assert_eq!(cases[1].case.domain, "source_code");
    }

    #[test]
    fn rows_pair_candidates_with_the_l2_final_state() {
        let cases = from_measurements(&measurements(), "default").unwrap();
        let rows = measure_all(&cases).unwrap();
        assert_eq!(rows[0]["disposition"], "compressed");
        assert_eq!(rows[0]["retention_form"], "offloaded-with-marker");
        assert_eq!(rows[0]["facts"]["passed"], rows[0]["facts"]["total"]);
        assert_eq!(rows[0]["l2_final"]["disposition"], "applied");
        assert!(rows[0]["gap"].as_str().unwrap().contains("Runtime"));
        assert_eq!(rows[1]["disposition"], "no-entry");
        assert!(
            rows[1]["gap"]
                .as_str()
                .unwrap()
                .contains("no isolated component")
        );
    }

    #[test]
    fn runtime_minimum_input_gate_cannot_drift_silently() {
        let source = include_str!("../../../crates/tokenless-runtime/src/entry.rs");
        assert!(source.contains(&format!(
            "const MIN_RESPONSE_CHARS: usize = {RUNTIME_MIN_RESPONSE_CHARS};"
        )));
    }

    #[test]
    fn invalid_fact_patterns_preserve_other_shared_rows() {
        let mut cases = from_measurements(&measurements(), "default").unwrap();
        cases[1].case.patterns.push("(?=unsupported)".into());
        let rows = measure_all(&cases).unwrap();
        assert_eq!(rows[0]["disposition"], "compressed");
        assert_eq!(rows[1]["disposition"], "invalid-evidence");
        assert!(rows[1]["tokens_after"].is_null());
    }

    #[test]
    fn schema2_reads_exact_deduplicated_input_bytes() {
        let mut doc = measurements();
        let input = doc["rows"][0]["row"]["input"].as_str().unwrap().to_owned();
        let fingerprint = format!("{:x}", Sha256::digest(input.as_bytes()));
        doc["text_store"] = json!({fingerprint.clone(): input});
        doc["rows"][0]["row"]["input"] = Value::Null;
        doc["rows"][0]["row"]["input_ref"] = json!(fingerprint);
        doc["rows"][0]["row"]["payload_hash"] = doc["rows"][0]["row"]["input_ref"].clone();
        assert!(!from_measurements(&doc, "default").unwrap().is_empty());
        doc["text_store"] = json!({fingerprint: "changed bytes"});
        assert!(from_measurements(&doc, "default").is_err());
    }

    #[test]
    fn domains_follow_runtime_detection() {
        assert_eq!(component_domain(Some("json"), "[]"), Some("json"));
        assert_eq!(
            component_domain(Some("search_results"), "a/b.rs\na/c.rs\na/d.rs\n"),
            Some("paths")
        );
        assert_eq!(
            component_domain(Some("search_results"), "a/b.rs:1:x\n"),
            Some("search")
        );
        assert_eq!(component_domain(Some("diff"), "diff --git"), None);
        assert_eq!(component_domain(None, "text"), None);
    }
}
