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
//! Verifies truncation, field dropping, and the reversible stash round-trip
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
fn debug_family_fields_are_dropped() {
    let obj = json!({
        "data": "keep",
        "debug": "x", "trace": "x", "traces": "x",
        "stack": "x", "stacktrace": "x", "logs": "x", "logging": "x"
    });
    let out = compress_json(&obj);
    let o = out.as_object().unwrap();
    assert_eq!(
        o["data"],
        json!("keep"),
        "preserved field must retain its value"
    );
    for k in [
        "debug",
        "trace",
        "traces",
        "stack",
        "stacktrace",
        "logs",
        "logging",
    ] {
        assert!(!o.contains_key(k), "{k} should be dropped");
    }
}

#[test]
fn nulls_are_dropped_by_default() {
    let out = compress_json(&json!({ "name": "t", "value": null, "count": 5 }));
    let o = out.as_object().unwrap();
    assert_eq!(o["name"], json!("t"), "name must retain its value");
    assert_eq!(o["count"], json!(5), "count must retain its value");
    assert!(!o.contains_key("value"));
}

#[test]
fn empty_fields_are_dropped_by_default() {
    let out = compress_json(&json!({
        "keep": "data", "es": "", "ea": [], "eo": {}
    }));
    let o = out.as_object().unwrap();
    assert_eq!(
        o["keep"],
        json!("data"),
        "preserved field must retain its value"
    );
    assert!(!o.contains_key("es"));
    assert!(!o.contains_key("ea"));
    assert!(!o.contains_key("eo"));
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
fn stashed_items_keep_fields_the_compressor_would_strip() {
    let store = Arc::new(InMemoryStore::new());
    let arr = json!([
        {
            "id": 1,
            "payload": "x".repeat(200),
            "debug": "stripped in kept item"
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
    // Kept item is compressed: debug stripped.
    assert!(a[0].get("debug").is_none());
    let hash = extract_hash(a.last().unwrap().as_str().unwrap()).unwrap();
    let recovered: Vec<Value> =
        serde_json::from_str(&store.retrieve(hash).unwrap().unwrap()).unwrap();
    // Stashed item is verbatim: debug preserved.
    assert!(
        recovered[0]["debug"]
            .as_str()
            .unwrap()
            .starts_with("survives in stash")
    );
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
