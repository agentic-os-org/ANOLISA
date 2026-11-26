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

//! Compression-rate regression guards.
//!
//! Pins the in-process savings rates over the canonical fixtures so any change
//! to the compressors (or fixtures) that erodes compression shows up as a
//! plain `cargo test` failure — traceable to the exact commit. Thresholds sit
//! a few points below the measured rates to absorb benign drift while still
//! catching real regressions.

use tokenless_bench::metrics::compression_metrics;

fn pct(report: &serde_json::Value, pointer: &str) -> f64 {
    report
        .pointer(pointer)
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| panic!("missing {pointer} in compression report"))
}

#[test]
fn compact_response_canonical_is_preserved_without_savings() {
    // Previously measured 36.3% by deleting diagnostic fields. The unchanged
    // compact fixture now measures preservation: 20,813 bytes in and out, 0%.
    let input = tokenless_bench::response_canonical();
    let (output, outcome) = tokenless_bench::compress_json_with(&input, Default::default(), None);
    assert_eq!(output, input);
    assert_eq!(outcome.output, serde_json::to_string(&input).unwrap());
    assert!(outcome.operations.is_empty());
    let report = compression_metrics();
    let saved = pct(&report, "/canonical/response/savings_pct");
    assert_eq!(saved, 0.0);
}

#[test]
fn pretty_response_canonical_compacts_without_omitting_values() {
    use tokenless_compressors::{JsonCompressionContext, JsonCompressor, JsonOperation};

    let original = tokenless_bench::response_canonical();
    let input = serde_json::to_string_pretty(&original).unwrap();
    let outcome = JsonCompressor::default()
        .compress(
            &input,
            &JsonCompressionContext {
                recovery: &tokenless_ccr::RecoveryMethod::None,
                stash: None,
                allow_toon: false,
                min_toon_chars: usize::MAX,
                allow_unrecoverable: false,
                embedded_html: false,
            },
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&outcome.output).unwrap(),
        original
    );
    assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
    assert!(outcome.stash_writes.is_empty());
    let before = tokenless_bench::metrics::estimate_tokens(&input);
    let after = tokenless_bench::metrics::estimate_tokens(&outcome.output);
    assert!(
        after < before,
        "pretty response must save tokens: {before} -> {after}"
    );
}

#[test]
fn schema_canonical_savings_at_least_40_pct() {
    let report = compression_metrics();
    let saved = pct(&report, "/canonical/schema/savings_pct");
    assert!(saved >= 40.0, "schema savings regressed: {saved}%");
}

#[test]
fn full_stack_savings_at_least_18_pct() {
    // Measured 19.5% with preserved response values and `toon-format` 0.7.0,
    // down from 50.3% when diagnostic fields were deleted. Fixtures and
    // stacking definitions match.
    let report = compression_metrics();
    let configs = report["stacking"]["configs"].as_array().unwrap();
    let full = configs
        .iter()
        .find(|c| c["config"] == "full_stack")
        .expect("full_stack config present");
    let saved = full["savings_pct"].as_f64().unwrap();
    assert!(saved >= 18.0, "full_stack savings regressed: {saved}%");
}

#[test]
fn no_config_exceeds_baseline() {
    // Scope: canonical fixtures only. Short-element arrays near the truncation
    // threshold (e.g. 33 items of 1 byte each) CAN legitimately inflate due to
    // the truncation marker overhead — that case is tested separately in
    // `array_33_short_elements_may_expand`.
    let report = compression_metrics();
    let baseline = report["stacking"]["baseline_tokens"].as_u64().unwrap();
    for row in report["stacking"]["configs"].as_array().unwrap() {
        let tokens = row["tokens"].as_u64().unwrap();
        assert!(
            tokens <= baseline,
            "config {} inflated: {tokens} > baseline {baseline}",
            row["config"]
        );
    }
}

#[test]
fn array_33_short_elements_may_expand() {
    use tokenless_bench::compress_json;

    // 33 numeric zeros — just above the default array truncation threshold (32).
    // For short elements, the truncation marker itself can cost more bytes than
    // the elements it replaces, causing net expansion. This test documents that
    // known behaviour rather than asserting non-expansion.
    let input = serde_json::Value::Array(vec![serde_json::json!(0); 33]);
    let output = compress_json(&input);
    let input_bytes = serde_json::to_string(&input).unwrap().len();
    let output_bytes = serde_json::to_string(&output).unwrap().len();
    // Must produce valid output regardless of expansion.
    assert!(output_bytes > 0);
    // The output array should be truncated: 32 kept + 1 marker = 33 entries.
    assert_eq!(output.as_array().unwrap().len(), 33);
    // Record expansion for visibility; not a failure condition.
    if output_bytes > input_bytes {
        eprintln!(
            "[info] array_33_short_elements: expansion {input_bytes} -> {output_bytes} bytes \
             (marker overhead on trivially-small elements)"
        );
    }
}

#[test]
fn content_domains_cover_current_engines_and_preserve_controls() {
    let report = tokenless_bench::domains::domain_metrics().unwrap();
    assert_eq!(report["headline_tokenizer"], "o200k_base");
    assert!(report["cost"].is_null());
    let rows = report["rows"].as_array().unwrap();
    // Frozen fixture/tokenizer bytes make these deterministic; intentional corpus
    // or tokenizer upgrades must update the counts after independent verification.
    let expected = [
        ("json-pretty", 6142, 4545),
        ("json-numeric-literals", 42, 33),
        ("schema-canonical", 320, 169),
        ("toon-rows", 1003, 706),
        ("search-3", 42, 42),
        ("paths-3", 27, 27),
        ("search-64", 896, 469),
        ("paths-64", 576, 364),
        ("search-256", 3584, 1813),
        ("paths-256", 2304, 1324),
        ("csv-64", 774, 708),
        ("tsv-64", 1034, 643),
        ("build-log-no-recovery", 1426, 1426),
        ("build-log-with-recovery", 1426, 116),
        ("html-page", 1162, 61),
        ("search-negative-control", 4, 4),
    ];
    assert_eq!(rows.len(), expected.len(), "fixture inventory changed");
    for (id, before, after) in expected {
        let row = rows
            .iter()
            .find(|row| row["sample_id"] == id)
            .expect("pinned fixture exists");
        assert_eq!(row["tokens_before"], before, "{id} input changed");
        assert_eq!(row["tokens_after"], after, "{id} output changed");
    }
    for domain in [
        "json",
        "schema",
        "toon",
        "search-results",
        "path-list",
        "tabular",
        "build-log",
        "html",
    ] {
        assert!(
            rows.iter().any(|row| row["domain"] == domain),
            "missing domain {domain}"
        );
    }
    for row in rows {
        assert_eq!(row["tokenizer"], "o200k_base");
        assert!(row["latency_ms"].as_f64().unwrap().is_finite());
        let checks = &row["checks"];
        let required: &[&str] = if row["sample_id"] == "search-negative-control" {
            &["rejected"]
        } else {
            match row["domain"].as_str().unwrap() {
                "json" => &["numeric_literals_retained"],
                "search-results" => &["all_numbered_row_suffixes_retained"],
                "path-list" => &["all_file_names_retained"],
                "tabular" => &["all_messages_retained"],
                "build-log" => &["diagnostic_retained", "all_omitted_blocks_retrievable"],
                "html" => &["required_facts_retained"],
                "schema" => &["output_is_object"],
                "toon" => &[],
                other => panic!("unregistered fixture domain {other}"),
            }
        };
        for key in required {
            assert_eq!(
                checks[key].as_bool(),
                Some(true),
                "{} lacks or fails {key}",
                row["sample_id"]
            );
        }
        if row["sample_id"] == "search-negative-control" {
            assert_eq!(row["tokens_before"], row["tokens_after"]);
            assert_eq!(row["checks"]["rejected"], true);
        }
        if checks["omitted_lines"].as_u64().unwrap_or(0) > 0 {
            assert_eq!(checks["all_omitted_blocks_retrievable"], true);
            assert_eq!(checks["recovered_blocks"], checks["omitted_blocks"]);
            assert_eq!(checks["recovered_lines"], checks["omitted_lines"]);
        }
    }
}

#[test]
fn component_exports_cannot_dump_private_payloads_to_stdout() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_component_cases"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--output"));
}

#[test]
fn private_fact_protocol_uses_rust_unicode_regex_semantics() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    for (output, passed) in [("hello", 1), ("1234", 0)] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_component_cases"))
            .arg("--check-facts-stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = serde_json::json!({"case": {
            "id": "unicode", "domain": "plain_text", "mode": "recovery", "input": "hello",
            "facts": [], "patterns": [r"\p{L}+"]
        }, "output": output});
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request.to_string().as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let score: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(score["passed"], passed);
        assert_eq!(score["total"], 1);
        assert!(score["input"].is_null() && score["output"].is_null());
    }
}

#[test]
fn repeated_payload_modes_do_not_inflate_independent_n() {
    use std::collections::BTreeSet;
    let report = tokenless_bench::domains::domain_metrics().unwrap();
    let rows = report["rows"].as_array().unwrap();
    let unique = rows
        .iter()
        .map(|row| row["payload_hash"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        report["independent_payloads"].as_u64().unwrap() as usize,
        unique.len()
    );
    assert!(
        unique.len() < rows.len(),
        "same log in two recovery modes is one independent payload"
    );
}
