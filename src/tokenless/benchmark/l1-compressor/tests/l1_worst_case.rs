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

//! These inputs are compression-friendly edge cases that must never expand
//! under the tested compressor configurations. This does NOT prove that no
//! expansion is possible on all inputs — see `l1_compression_rate.rs` test
//! `array_33_short_elements_may_expand` for a documented expansion case.
//!
//! For inputs that carry genuine redundancy (long strings, big arrays,
//! debug/null/empty noise, over-deep nesting) the compressed serialization
//! must be no larger than the original. Minimal inputs with nothing to remove
//! are returned unchanged (equal size). 9 tests.

use serde_json::{Value, json};
use tokenless_bench::compress_json;
use tokenless_schema::SchemaCompressor;

/// Serialized byte length — the proxy for token cost used across these tests.
fn len(v: &Value) -> usize {
    serde_json::to_string(v).unwrap().len()
}

fn assert_not_expanded(input: &Value, output: &Value) {
    assert!(
        len(output) <= len(input),
        "compression expanded output: {} -> {} bytes",
        len(input),
        len(output)
    );
}

#[test]
fn long_string_shrinks() {
    let input = json!({ "log": "detail ".repeat(5000) });
    let output = compress_json(&input);
    assert_not_expanded(&input, &output);
}

#[test]
fn large_object_array_shrinks() {
    let items: Vec<Value> = (0..500)
        .map(|i| json!({ "id": i, "name": format!("record-{i}"), "note": "some substantial payload text" }))
        .collect();
    let input = Value::Array(items);
    let output = compress_json(&input);
    assert_not_expanded(&input, &output);
}

#[test]
fn debug_fields_removal_shrinks() {
    let input = json!({
        "result": "ok",
        "debug": "x".repeat(2000),
        "stacktrace": "y".repeat(2000),
        "logs": "z".repeat(2000)
    });
    let output = compress_json(&input);
    assert_not_expanded(&input, &output);
}

#[test]
fn null_removal_shrinks() {
    let mut obj = serde_json::Map::new();
    for i in 0..500 {
        obj.insert(format!("k{i}"), Value::Null);
    }
    obj.insert("keep".into(), json!("value"));
    let input = Value::Object(obj);
    let output = compress_json(&input);
    assert_not_expanded(&input, &output);
}

#[test]
fn empty_field_removal_shrinks() {
    let mut obj = serde_json::Map::new();
    for i in 0..500 {
        obj.insert(format!("k{i}"), json!(""));
    }
    obj.insert("keep".into(), json!("value"));
    let input = Value::Object(obj);
    let output = compress_json(&input);
    assert_not_expanded(&input, &output);
}

#[test]
fn over_deep_nesting_collapses_smaller() {
    let mut input = json!({ "leaf": "x".repeat(5000) });
    for _ in 0..50 {
        input = json!({ "child": input });
    }
    let output = compress_json(&input);
    assert_not_expanded(&input, &output);
}

#[test]
fn schema_long_description_shrinks() {
    let input = json!({
        "function": {
            "name": "t",
            "description": "word ".repeat(2000),
            "parameters": { "type": "object", "properties": {
                "p": { "type": "string", "description": "word ".repeat(2000) } } }
        }
    });
    let output = SchemaCompressor::new().compress(&input);
    assert_not_expanded(&input, &output);
}

#[test]
fn minimal_schema_is_returned_unchanged() {
    let input =
        json!({ "function": { "name": "t", "parameters": {"type":"object","properties":{}} } });
    let output = SchemaCompressor::new().compress(&input);
    // Nothing to remove → returned verbatim, so size is exactly equal.
    assert_eq!(len(&output), len(&input));
}

#[test]
fn combined_noise_response_shrinks() {
    let items: Vec<Value> = (0..200)
        .map(|i| {
            json!({ "id": i, "debug": "d".repeat(200), "empty": "", "null_field": null,
                          "msg": "a fairly long descriptive message repeated across records" })
        })
        .collect();
    let input = json!({ "results": items, "trace": "t".repeat(5000) });
    let output = compress_json(&input);
    assert_not_expanded(&input, &output);
}
