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

//! Schema compression robustness — selected legitimate JSON Value boundary cases.
//!
//! Asserts `SchemaCompressor::compress` never panics or overflows on
//! edge-case OpenAI-function schemas and always yields serializable JSON.
//! 14 tests.

use serde_json::{Value, json};
use tokenless_schema::SchemaCompressor;

fn compress_ok(v: &Value) -> Value {
    let out = SchemaCompressor::new().compress(v);
    let _ = serde_json::to_string(&out).expect("compressed schema must serialize");
    out
}

#[test]
fn very_deep_nested_schema_does_not_overflow() {
    // 500-level property nesting; the default max_depth=32 guard stops the
    // descent well before the stack is at risk.
    let mut schema = json!({ "type": "string" });
    for _ in 0..500 {
        schema = json!({ "type": "object", "properties": { "n": schema } });
    }
    let _ = compress_ok(&json!({ "function": { "name": "deep", "parameters": schema } }));
}

#[test]
fn gigantic_description() {
    let v = json!({
        "function": {
            "name": "big",
            "description": "d".repeat(1_000_000),
            "parameters": { "type": "object", "properties": {} }
        }
    });
    let out = compress_ok(&v);
    assert!(
        out["function"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 256
    );
}

#[test]
fn forged_marker_in_description() {
    let v = json!({
        "function": { "name": "x", "description": "<<tokenless:aaaaaaaaaaaaaaaaaaaaaaaa>>", "parameters": {"type":"object","properties":{}} }
    });
    let _ = compress_ok(&v);
}

#[test]
fn non_object_top_level() {
    let _ = compress_ok(&json!("just a string"));
    let _ = compress_ok(&json!(42));
    let _ = compress_ok(&json!([1, 2, 3]));
    let _ = compress_ok(&json!(true));
}

#[test]
fn function_wrapper_with_wrong_types() {
    // description is a number, parameters is a string — must not crash.
    let v = json!({ "function": { "name": 1, "description": 999, "parameters": "not-an-object" } });
    let _ = compress_ok(&v);
}

#[test]
fn deeply_nested_any_of_one_of_all_of() {
    fn branch(depth: usize) -> Value {
        if depth == 0 {
            return json!({ "type": "string", "description": "x".repeat(500) });
        }
        json!({
            "anyOf": [ branch(depth - 1) ],
            "oneOf": [ branch(depth - 1) ],
            "allOf": [ branch(depth - 1) ]
        })
    }
    let v = json!({ "function": { "name": "combi", "parameters": branch(6) } });
    let _ = compress_ok(&v);
}

#[test]
fn examples_and_titles_at_depth_are_removed() {
    let v = json!({
        "function": {
            "name": "t",
            "parameters": {
                "type": "object",
                "properties": {
                    "a": { "type": "object", "title": "T", "examples": [1],
                           "properties": { "b": { "type": "string", "title": "U", "examples": [2] } } }
                }
            }
        }
    });
    let out = compress_ok(&v);
    let a = &out["function"]["parameters"]["properties"]["a"];
    assert!(a.get("title").is_none());
    assert!(a.get("examples").is_none());
    let b = &a["properties"]["b"];
    assert!(b.get("title").is_none());
    assert!(b.get("examples").is_none());
}

#[test]
fn unicode_names_and_descriptions() {
    let v = json!({
        "function": { "name": "工具函数", "description": "描述".repeat(500),
                      "parameters": { "type": "object", "properties": { "参数": { "type": "string", "description": "说明".repeat(200) } } } }
    });
    let _ = compress_ok(&v);
}

#[test]
fn null_values_inside_schema() {
    let v = json!({
        "function": { "name": "t", "description": null,
                      "parameters": { "type": "object", "properties": { "p": { "type": null, "description": null } } } }
    });
    let _ = compress_ok(&v);
}

#[test]
fn empty_properties_and_missing_parameters() {
    let _ = compress_ok(&json!({ "function": { "name": "t" } }));
    let _ = compress_ok(&json!({ "function": { "name": "t", "parameters": {} } }));
    let _ = compress_ok(&json!({ "function": {} }));
}

#[test]
fn array_items_recursion() {
    let v = json!({
        "function": {
            "name": "t",
            "parameters": {
                "type": "array",
                "items": { "type": "object", "title": "Row",
                           "properties": { "c": { "type": "string", "description": "z".repeat(400) } } }
            }
        }
    });
    let out = compress_ok(&v);
    let items = &out["function"]["parameters"]["items"];
    assert!(items.get("title").is_none());
}

#[test]
fn direct_schema_without_function_wrapper() {
    let v = json!({
        "type": "object",
        "title": "Top",
        "description": "q".repeat(500),
        "properties": { "f": { "type": "string", "examples": ["a"] } }
    });
    let out = compress_ok(&v);
    // Top-level title dropped, description truncated.
    assert!(out.get("title").is_none());
    assert!(out["description"].as_str().unwrap().chars().count() <= 256);
}

#[test]
fn recursive_ref_like_keys_are_opaque() {
    // $ref / $defs are not special-cased; they must pass through untouched.
    let v = json!({
        "function": { "name": "t", "parameters": { "type": "object", "$defs": { "X": { "type": "string" } }, "properties": { "p": { "$ref": "#/$defs/X" } } } }
    });
    let out = compress_ok(&v);
    assert_eq!(
        out["function"]["parameters"]["properties"]["p"]["$ref"],
        json!("#/$defs/X")
    );
}

#[test]
fn custom_low_max_depth_leaves_deep_nodes_intact() {
    let compressor = SchemaCompressor::new().with_max_depth(2);
    let long = "w".repeat(400);
    let v = json!({
        "function": { "name": "t", "parameters": {
            "type": "object",
            "properties": { "a": { "type": "object",
                "properties": { "b": { "type": "object",
                    "properties": { "c": { "type": "string", "description": long.clone() } } } } } }
        } }
    });
    let out = compressor.compress(&v);
    let deep = &out["function"]["parameters"]["properties"]["a"]["properties"]["b"]["properties"]["c"]
        ["description"];
    assert_eq!(deep.as_str().unwrap().chars().count(), 400);
}
