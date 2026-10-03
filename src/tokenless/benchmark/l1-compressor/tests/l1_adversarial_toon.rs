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

//! Adversarial TOON encoding — robustness against hostile / edge JSON.
//!
//! Asserts `encode_default` / `decode_default` never panic on pathological
//! input. For well-formed JSON, encoding succeeds and the result decodes back
//! to a value; for malformed TOON text, decoding fails gracefully (Err, not a
//! panic). 10 tests.

use serde_json::{Value, json};

/// Encode must succeed and the result must decode back to the original value.
fn encode_then_decode_ok(v: &Value) {
    let toon = toon_format::encode_default(v).expect("encode must succeed for valid input");
    let decoded: Value =
        toon_format::decode_default(&toon).expect("decode must succeed for valid encoding");
    assert_eq!(&decoded, v, "roundtrip must preserve value");
}

#[test]
fn deeply_nested_object_encodes() {
    let mut v = json!({ "leaf": 1 });
    for _ in 0..200 {
        v = json!({ "child": v });
    }
    // 200 levels of nesting may exceed encoder limits. Either encode+decode
    // round-trips cleanly, or the encoder returns a well-formed Err — both are
    // acceptable, but a panic or silent data loss is not.
    match toon_format::encode_default(&v) {
        Ok(toon) => {
            let decoded: Value =
                toon_format::decode_default(&toon).expect("decode must succeed for valid encoding");
            assert_eq!(decoded, v, "roundtrip must preserve value");
        }
        Err(_) => {
            // Graceful rejection of overly-deep input is acceptable.
        }
    }
}

#[test]
fn large_uniform_table_encodes() {
    let rows: Vec<Value> = (0..5000)
        .map(|i| json!({ "id": i, "name": format!("r{i}"), "v": i * 3 }))
        .collect();
    encode_then_decode_ok(&json!({ "rows": rows }));
}

#[test]
fn unicode_keys_and_values_roundtrip() {
    let v = json!({ "键": "值", "列表": ["甲", "乙", "丙"] });
    let toon = toon_format::encode_default(&v).expect("encode");
    let back = toon_format::decode_default::<Value>(&toon).expect("decode");
    assert_eq!(back, v);
}

#[test]
fn special_characters_needing_escape_roundtrip() {
    let v = json!({
        "comma": "a,b,c",
        "colon": "k: v",
        "quote": "say \"hi\"",
        "brackets": "[not an array]"
    });
    let toon = toon_format::encode_default(&v).expect("encode");
    let back = toon_format::decode_default::<Value>(&toon).expect("decode");
    assert_eq!(back, v);
}

#[test]
fn empty_and_null_containers_encode() {
    encode_then_decode_ok(&json!({ "a": [], "b": {}, "c": null, "d": "" }));
}

#[test]
fn mixed_type_array_encodes() {
    encode_then_decode_ok(&json!([1, "two", 3.5, true, null, { "k": "v" }, [9, 8]]));
}

#[test]
fn numeric_precision_roundtrip() {
    let v = json!({ "big": 9007199254740991i64, "neg": -12345, "float": std::f64::consts::PI });
    let toon = toon_format::encode_default(&v).expect("encode");
    let back = toon_format::decode_default::<Value>(&toon).expect("decode");
    assert_eq!(back, v);
}

#[test]
fn value_that_looks_like_toon_syntax_roundtrip() {
    // A string whose content mimics TOON's own indentation/key syntax must not
    // be misparsed on the way back.
    let v = json!({ "payload": "a: 1\n  b: 2\n- item\n[3]{x}" });
    let toon = toon_format::encode_default(&v).expect("encode");
    let back = toon_format::decode_default::<Value>(&toon).expect("decode");
    assert_eq!(back, v);
}

#[test]
fn decode_garbage_does_not_panic() {
    // Random non-TOON bytes: decode must return Err (or an Ok scalar), never
    // panic.
    for garbage in ["@@@###$$$", "\u{0000}\u{0001}", "}{][:,", "  \t\n  "] {
        let _ = toon_format::decode_default::<Value>(garbage);
    }
}

#[test]
fn decode_empty_string_does_not_panic() {
    let _ = toon_format::decode_default::<Value>("");
}
