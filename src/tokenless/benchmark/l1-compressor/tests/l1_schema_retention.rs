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

//! Schema compression quality — field retention & truncation correctness.
//!
//! Verifies that SchemaCompressor keeps semantically-required fields intact
//! while shrinking descriptions and stripping noise. 8 tests.

use serde_json::json;
use tokenless_schema::SchemaCompressor;

#[test]
fn protected_fields_are_preserved() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "function": {
            "name": "my_function",
            "parameters": {
                "type": "object",
                "required": ["field1"],
                "properties": {
                    "field1": {
                        "type": "string",
                        "enum": ["a", "b", "c"],
                        "default": "a",
                        "const": "fixed"
                    }
                }
            }
        }
    });
    let out = compressor.compress(&schema);
    assert_eq!(out["function"]["name"], "my_function");
    assert_eq!(out["function"]["parameters"]["type"], "object");
    assert_eq!(
        out["function"]["parameters"]["required"],
        json!(["field1"]),
        "required array must preserve exact content"
    );
    let f1 = &out["function"]["parameters"]["properties"]["field1"];
    assert_eq!(
        f1["enum"],
        json!(["a", "b", "c"]),
        "enum array must preserve exact content"
    );
    assert_eq!(f1["default"], "a");
    assert_eq!(f1["const"], "fixed");
}

#[test]
fn titles_and_examples_are_dropped() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "function": {
            "name": "t",
            "title": "Fn Title",
            "parameters": {
                "type": "object",
                "title": "Params Title",
                "properties": {
                    "f": { "type": "string", "title": "Field", "examples": ["x", "y"] }
                }
            }
        }
    });
    let out = compressor.compress(&schema);
    assert!(out["function"].get("title").is_none());
    assert!(out["function"]["parameters"].get("title").is_none());
    let f = &out["function"]["parameters"]["properties"]["f"];
    assert!(f.get("title").is_none());
    assert!(f.get("examples").is_none());
}

#[test]
fn function_description_truncated_to_default_limit() {
    let compressor = SchemaCompressor::new();
    let long = "word ".repeat(200);
    let schema = json!({
        "function": { "name": "t", "description": long, "parameters": {"type": "object", "properties": {}} }
    });
    let out = compressor.compress(&schema);
    let desc = out["function"]["description"].as_str().unwrap();
    assert!(
        desc.chars().count() <= 256,
        "func desc must be <= 256 chars"
    );
}

#[test]
fn param_description_truncated_to_default_limit() {
    let compressor = SchemaCompressor::new();
    let long = "word ".repeat(200);
    let schema = json!({
        "function": {
            "name": "t",
            "parameters": {
                "type": "object",
                "properties": { "p": { "type": "string", "description": long } }
            }
        }
    });
    let out = compressor.compress(&schema);
    let desc = out["function"]["parameters"]["properties"]["p"]["description"]
        .as_str()
        .unwrap();
    assert!(
        desc.chars().count() <= 160,
        "param desc must be <= 160 chars"
    );
}

#[test]
fn custom_func_desc_max_len_is_respected() {
    let compressor = SchemaCompressor::new().with_func_desc_max_len(50);
    let long = "A".repeat(100);
    let schema = json!({
        "function": { "name": "t", "description": long, "parameters": {"type": "object", "properties": {}} }
    });
    let out = compressor.compress(&schema);
    let desc = out["function"]["description"].as_str().unwrap();
    assert!(desc.chars().count() <= 50);
}

#[test]
fn max_depth_stops_recursion() {
    // Below the depth limit descriptions stay untouched.
    let compressor = SchemaCompressor::new().with_max_depth(5);
    let long = "x".repeat(400);
    let mut schema = json!({ "type": "string", "description": long.clone() });
    for _ in 0..50 {
        schema = json!({
            "type": "object",
            "description": long.clone(),
            "properties": { "nested": schema }
        });
    }
    let out = compressor.compress(&schema);
    // Top-level (depth 0) truncated.
    assert!(out["description"].as_str().unwrap().chars().count() <= 256);
    // Deep node (well past max_depth) keeps the original 400-char description.
    let mut node = &out;
    for _ in 0..10 {
        node = &node["properties"]["nested"];
    }
    assert_eq!(node["description"].as_str().unwrap().chars().count(), 400);
}

#[test]
fn cjk_description_no_panic_and_within_limit() {
    let compressor = SchemaCompressor::new();
    let cjk = "中".repeat(300);
    let out = compressor.truncate_description(&cjk, 256);
    assert!(out.chars().count() <= 256);
    assert!(out.chars().all(|c| c == '中'));
}

#[test]
fn empty_and_null_schema_do_not_panic() {
    let compressor = SchemaCompressor::new();
    assert!(compressor.compress(&json!({})).is_object());
    assert!(compressor.compress(&serde_json::Value::Null).is_null());
    assert!(compressor.compress(&json!({"function": {}}))["function"].is_object());
}
