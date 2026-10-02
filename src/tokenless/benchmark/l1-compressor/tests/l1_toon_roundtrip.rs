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

//! TOON round-trip correctness — encode then decode must preserve semantics.
//!
//! TOON is a lossy-looking but semantically-reversible encoding. These 8
//! tests assert JSON -> TOON -> JSON equals the original value for a range of
//! shapes (flat, nested, arrays, mixed types, CJK, special chars, empty).

use serde_json::{Value, json};

/// encode then decode, asserting the recovered value equals the input.
fn assert_roundtrip(value: &Value) {
    let encoded = toon_format::encode_default(value).expect("encode");
    let decoded = toon_format::decode_default::<Value>(&encoded).expect("decode");
    assert_eq!(&decoded, value, "roundtrip mismatch\nTOON:\n{encoded}");
}

#[test]
fn roundtrip_flat_object() {
    assert_roundtrip(&json!({ "name": "Ada", "born": 1815, "active": true }));
}

#[test]
fn roundtrip_nested_object() {
    assert_roundtrip(&json!({
        "a": { "b": { "c": { "d": 1 } } },
        "e": [1, 2, 3]
    }));
}

#[test]
fn roundtrip_uniform_array_of_objects() {
    assert_roundtrip(&json!({
        "rows": [
            { "id": 1, "name": "x" },
            { "id": 2, "name": "y" },
            { "id": 3, "name": "z" }
        ]
    }));
}

#[test]
fn roundtrip_mixed_scalar_types() {
    assert_roundtrip(&json!({
        "s": "text", "i": 42, "f": 3.5, "b": false, "n": null
    }));
}

#[test]
fn roundtrip_cjk_strings() {
    assert_roundtrip(&json!({ "msg": "你好世界", "list": ["第一", "第二", "第三"] }));
}

#[test]
fn roundtrip_special_characters() {
    assert_roundtrip(&json!({
        "quote": "he said \"hi\"",
        "comma": "a,b,c",
        "colon": "key: value",
        "newline": "line1\nline2"
    }));
}

#[test]
fn roundtrip_empty_containers() {
    assert_roundtrip(&json!({ "arr": [], "obj": {}, "s": "" }));
}

#[test]
fn roundtrip_array_of_scalars() {
    assert_roundtrip(&json!([10, 20, 30, 40, 50]));
}
