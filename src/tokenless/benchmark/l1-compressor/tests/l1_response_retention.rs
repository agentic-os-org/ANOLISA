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

//! Response compression quality — information retention & reversibility.
//!
//! Verifies truncation, field preservation, and the reversible stash round-trip
//! preserve exactly what they should. 11 tests.

use serde_json::{Value, json};
use std::sync::Arc;
use tokenless_bench::{compress_json, compress_json_with, response_record_reduction};
use tokenless_ccr::{InMemoryStore, StashStore, extract_hash};
use tokenless_compressors::{JsonCompressionConfig, JsonOperation};

#[test]
fn record_reduction_fixture_preserves_the_middle_anomaly() {
    let store = InMemoryStore::new();
    let fixture = response_record_reduction();
    let (out, outcome) =
        compress_json_with(&fixture, JsonCompressionConfig::default(), Some(&store));

    assert_eq!(outcome.operations, [JsonOperation::RecordReduction]);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert!(out.as_array().unwrap().iter().any(|record| {
        record.get("id") == Some(&json!(31)) && record.get("status") == Some(&json!("failed"))
    }));
}

#[test]
fn string_truncation_adds_marker() {
    let long = "This is a very long string that should be truncated";
    let (out, _) = compress_json_with(
        &json!(long),
        JsonCompressionConfig {
            truncate_strings_at: 20,
            ..JsonCompressionConfig::default()
        },
        None,
    );
    let s = out.as_str().unwrap();
    assert!(s.contains("… (truncated)"));
}

#[test]
fn string_within_limit_is_untouched() {
    let short = "short value";
    let out = compress_json(&json!(short));
    assert_eq!(out, json!(short));
}

#[test]
fn array_truncation_default_limit_is_32() {
    let arr: Vec<String> = (1..=50)
        .map(|index| format!("item-{index}-{}", "x".repeat(80)))
        .collect();
    let out = compress_json(&json!(arr));
    // 32 head items + 1 marker + 8 tail items (default preserve).
    assert_eq!(out.as_array().unwrap().len(), 41);
}

#[test]
fn array_truncation_custom_limit() {
    let arr: Vec<String> = (1..=10)
        .map(|index| format!("item-{index}-{}", "x".repeat(80)))
        .collect();
    let (out, _) = compress_json_with(
        &json!(arr),
        JsonCompressionConfig {
            truncate_arrays_at: 3,
            array_tail_preserve: 0,
            ..JsonCompressionConfig::default()
        },
        None,
    );
    let a = out.as_array().unwrap();
    assert_eq!(a.len(), 4);
    assert!(a[3].as_str().unwrap().contains("truncated"));
}

#[test]
fn debug_family_fields_are_preserved() {
    let obj = json!({
        "data": "keep",
        "debug": "x", "trace": "x", "traces": "x",
        "stack": "x", "stacktrace": "x", "logs": "x", "logging": "x"
    });
    let out = compress_json(&obj);
    assert_eq!(out, obj);
}

#[test]
fn nulls_are_preserved() {
    let input = json!({ "name": "t", "value": null, "count": 5 });
    assert_eq!(compress_json(&input), input);
}

#[test]
fn empty_fields_are_preserved() {
    let input = json!({
        "keep": "data", "es": "", "ea": [], "eo": {}
    });
    assert_eq!(compress_json(&input), input);
}

#[test]
fn stash_round_trip_recovers_dropped_items_verbatim() {
    let store = Arc::new(InMemoryStore::new());
    let values = ["a", "b", "c", "d", "e"].map(|value| format!("{value}-{}", "x".repeat(80)));
    let arr = json!(values);
    let (out, outcome) = compress_json_with(
        &arr,
        JsonCompressionConfig {
            truncate_arrays_at: 2,
            array_tail_preserve: 0,
            ..JsonCompressionConfig::default()
        },
        Some(store.as_ref()),
    );
    let a = out.as_array().unwrap();
    assert_eq!(a[0], json!(values[0]));
    assert_eq!(a[1], json!(values[1]));
    let hash = extract_hash(a.last().unwrap().as_str().unwrap()).unwrap();
    let recovered: Vec<String> =
        serde_json::from_str(&store.retrieve(hash).unwrap().unwrap()).unwrap();
    assert_eq!(recovered, values[2..]);
    assert_eq!(outcome.stash_writes.len(), 1);
}

#[test]
fn retained_and_stashed_items_both_keep_diagnostic_fields() {
    let store = Arc::new(InMemoryStore::new());
    let arr = json!([
        {
            "id": 1,
            "payload": "x".repeat(200),
            "debug": "retained in kept item"
        },
        {
            "id": 2,
            "payload": "x".repeat(200),
            "debug": "survives in stash"
        }
    ]);
    let (out, _) = compress_json_with(
        &arr,
        JsonCompressionConfig {
            truncate_arrays_at: 1,
            array_tail_preserve: 0,
            ..JsonCompressionConfig::default()
        },
        Some(store.as_ref()),
    );
    let a = out.as_array().unwrap();
    assert_eq!(a[0], arr[0]);
    let hash = extract_hash(a.last().unwrap().as_str().unwrap()).unwrap();
    let recovered: Vec<Value> =
        serde_json::from_str(&store.retrieve(hash).unwrap().unwrap()).unwrap();
    assert_eq!(recovered[0], arr[1]);
}

#[test]
fn no_marker_when_array_within_limit() {
    let store = Arc::new(InMemoryStore::new());
    let (out, outcome) = compress_json_with(
        &json!([1, 2, 3]),
        JsonCompressionConfig {
            truncate_arrays_at: 10,
            ..JsonCompressionConfig::default()
        },
        Some(store.as_ref()),
    );
    assert!(out.as_array().unwrap().iter().all(|v| v.is_number()));
    assert_eq!(store.len(), 0);
    assert!(outcome.stash_writes.is_empty());
}
