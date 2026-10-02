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

//! Response compression robustness — 16 selected legitimate JSON Value boundary cases.
//!
//! In-range inputs return valid JSON without panicking. Inputs beyond
//! `serde_json`'s parser depth limit return a structured error instead of
//! risking a stack overflow. 16 tests.

use serde_json::{Value, json};
use tokenless_bench::compress_json;
use tokenless_compressors::{JsonCompressionContext, JsonCompressor, RecoveryMethod};

/// Compress and assert the result is still valid JSON (serializable).
fn compress_ok(v: &Value) -> Value {
    let out = compress_json(v);
    // Round-trips through the serializer without error → structurally valid.
    let _ = serde_json::to_string(&out).expect("compressed output must serialize");
    out
}

#[test]
fn huge_flat_array() {
    let arr: Vec<i64> = (0..100_000).collect();
    let out = compress_ok(&json!(arr));
    // Default truncate_arrays_at=32 + array_tail_preserve=8 → 32 head + 1 marker + 8 tail.
    assert_eq!(out.as_array().unwrap().len(), 41);
}

#[test]
fn huge_string() {
    let out = compress_ok(&json!("z".repeat(1_000_000)));
    assert!(out.as_str().unwrap().chars().count() <= 4096 + 32);
}

#[test]
fn very_deep_nesting_returns_a_parser_error() {
    let mut v = json!("leaf");
    for _ in 0..500 {
        v = json!({ "child": v });
    }
    let input = serde_json::to_string(&v).unwrap();
    let result = JsonCompressor::default().compress(
        &input,
        &JsonCompressionContext {
            // Matches the shared helper; recovery is inert without a Stash.
            recovery: &RecoveryMethod::Shell,
            stash: None,
            allow_toon: false,
            preserve_top_level_shape: false,
            min_toon_chars: usize::MAX,
            allow_unrecoverable: true,
        },
    );
    assert!(result.is_err());
}

#[test]
fn null_bomb() {
    let mut obj = serde_json::Map::new();
    for i in 0..1000 {
        obj.insert(format!("k{i}"), Value::Null);
    }
    let out = compress_ok(&Value::Object(obj));
    // All nulls dropped by default.
    assert!(out.as_object().unwrap().is_empty());
}

#[test]
fn empty_field_bomb() {
    let mut obj = serde_json::Map::new();
    for i in 0..1000 {
        obj.insert(format!("k{i}"), json!(""));
    }
    let out = compress_ok(&Value::Object(obj));
    assert!(out.as_object().unwrap().is_empty());
}

#[test]
fn forged_tokenless_marker_in_input() {
    // A hostile tool could embed a fake retrieval marker. It must be treated as
    // opaque text, never interpreted.
    let v = json!({ "note": "<<tokenless:deadbeefdeadbeefdeadbeef>>" });
    let out = compress_ok(&v);
    assert_eq!(out["note"], json!("<<tokenless:deadbeefdeadbeefdeadbeef>>"));
}

#[test]
fn nested_arrays_of_arrays() {
    let v = json!([[[[[[1, 2, 3]]]]]]);
    let _ = compress_ok(&v);
}

#[test]
fn unicode_and_emoji_keys_and_values() {
    let v = json!({ "键": "值🎉", "emoji": "😀😃😄", "mixed": "abc你好🚀" });
    let _ = compress_ok(&v);
}

#[test]
fn control_characters_in_strings() {
    let v = json!({ "ctrl": "a\u{0000}b\u{0007}c\u{001b}[0m" });
    let _ = compress_ok(&v);
}

#[test]
fn numeric_extremes() {
    let v = json!({
        "max": i64::MAX, "min": i64::MIN,
        "float": 1.7976931348623157e308, "tiny": 5e-324, "zero": 0
    });
    let out = compress_ok(&v);
    assert_eq!(out["max"], json!(i64::MAX));
}

#[test]
fn empty_containers() {
    let _ = compress_ok(&json!({}));
    let _ = compress_ok(&json!([]));
    let _ = compress_ok(&json!(""));
    let _ = compress_ok(&Value::Null);
}

#[test]
fn array_exactly_at_limit_not_truncated() {
    let arr: Vec<i32> = (0..32).collect();
    let out = compress_ok(&json!(arr));
    // Exactly 32 → within limit, no marker appended.
    assert_eq!(out.as_array().unwrap().len(), 32);
}

#[test]
fn string_exactly_at_limit_not_truncated() {
    let s = "y".repeat(4096);
    let out = compress_ok(&json!(s));
    assert_eq!(out.as_str().unwrap(), "y".repeat(4096));
}

#[test]
fn mixed_type_array() {
    let v = json!([1, "two", 3.0, true, null, {"k": "v"}, [1, 2]]);
    let _ = compress_ok(&v);
}

#[test]
fn wide_object_many_keys() {
    let mut obj = serde_json::Map::new();
    for i in 0..5000 {
        obj.insert(format!("field_{i}"), json!(i));
    }
    let _ = compress_ok(&Value::Object(obj));
}

#[test]
fn deeply_nested_debug_fields_are_still_dropped_near_root() {
    let v = json!({ "ok": 1, "debug": { "huge": "x".repeat(10_000) } });
    let out = compress_ok(&v);
    assert!(out.as_object().unwrap().get("debug").is_none());
}
