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
fn response_canonical_lossless_savings_at_least_35_pct() {
    let report = compression_metrics();
    let saved = pct(&report, "/canonical/response/savings_pct");
    assert!(saved >= 35.0, "response savings regressed: {saved}%");
}

#[test]
fn schema_canonical_savings_at_least_40_pct() {
    let report = compression_metrics();
    let saved = pct(&report, "/canonical/schema/savings_pct");
    assert!(saved >= 40.0, "schema savings regressed: {saved}%");
}

#[test]
fn full_stack_savings_at_least_49_pct() {
    let report = compression_metrics();
    let configs = report["stacking"]["configs"].as_array().unwrap();
    let full = configs
        .iter()
        .find(|c| c["config"] == "full_stack")
        .expect("full_stack config present");
    let saved = full["savings_pct"].as_f64().unwrap();
    assert!(saved >= 49.0, "full_stack savings regressed: {saved}%");
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
