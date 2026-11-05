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

//! TOON spec v4.4 regressions for the `toon-format` 0.5 -> 0.7 upgrade.
//!
//! 0.7.0 implements TOON spec v4.4. Two encoded forms are new and are the
//! whole point of the upgrade, because 0.5.0 expanded both of them *past*
//! compact JSON and so never won the candidate gate:
//!
//! * nested field groups — `users[2]{id,profile{name,active}}:`
//! * keyed tabular maps — `users[2:]{name,active}:`
//!
//! Three 0.5.0 defects are fixed and pinned here:
//!
//! * `[{}, {}]` encoded to `items[2]{}:`, a document no version can decode
//!   (`Field list cannot be empty for tabular arrays`);
//! * `1e20` decoded back as the *string* `"1e20"`;
//! * non-ASCII keys were emitted unquoted.
//!
//! Two further renderings changed without being defects: an empty array is
//! now inline (`items: []` instead of `items[0]:`) and string elements that
//! need no quoting are emitted bare. Both round-trip, and their re-baselined
//! expectations live in `tests/test-toon-full.sh` and the
//! `post_tool/bash_array_truncation/cosh-ng` hook golden.
//!
//! The mixed-version cases replay the documents 0.5.0 actually produced for
//! the same inputs (captured before the upgrade) rather than depending on the
//! superseded crate: a rollback leaves that text on disk, so the 0.7.0
//! decoder must still read it, and the new headers must stay pinned so an
//! accidental encoder change is caught here instead of by a consumer running
//! a pre-0.6 decoder.
//!
//! Precision is *not* fixed by this upgrade and stays out of scope: TOON
//! still cannot carry `1e-400`, `-0.0` or an exponent spelling such as
//! `1.0e3`, and out-of-`u64` literals survive only as the `f64` serde_json
//! already parsed them into. The JSON domain rejects those candidates by
//! value equality and keeps exact JSON; the last test asserts the premise of
//! that guard still holds on 0.7.0.

use serde_json::{Value, json};

/// Encode then decode, asserting the recovered value equals the input.
fn assert_roundtrip(value: &Value) -> String {
    let encoded = toon_format::encode_default(value).expect("encode");
    let candidate = encoded.trim_end().to_owned();
    let decoded = toon_format::decode_default::<Value>(&candidate).expect("decode");
    assert_eq!(&decoded, value, "roundtrip mismatch\nTOON:\n{candidate}");
    candidate
}

fn nested_records(count: usize) -> Value {
    let users: Vec<Value> = (0..count)
        .map(|index| {
            json!({
                "id": index,
                "profile": { "name": format!("user{index}"), "active": index % 2 == 0 }
            })
        })
        .collect();
    json!({ "users": users })
}

fn keyed_records(count: usize) -> Value {
    let mut users = serde_json::Map::new();
    for index in 0..count {
        users.insert(
            format!("u{index}"),
            json!({ "name": format!("user{index}"), "active": index % 2 == 0 }),
        );
    }
    json!({ "users": Value::Object(users) })
}

// --- the two new spec v4.4 forms -------------------------------------------

#[test]
fn nested_record_groups_round_trip() {
    let encoded = assert_roundtrip(&nested_records(2));
    assert_eq!(
        encoded,
        "users[2]{id,profile{name,active}}:\n  0,user0,true\n  1,user1,false"
    );
}

#[test]
fn keyed_record_maps_round_trip() {
    let encoded = assert_roundtrip(&keyed_records(2));
    assert_eq!(
        encoded,
        "users[2:]{name,active}:\n  u0: user0,true\n  u1: user1,false"
    );
}

#[test]
fn nested_records_encode_smaller_than_compact_json() {
    // The 0.5.0 encoder expanded every nested record into its own indented
    // block, so this shape lost the size gate and fell back to JSON. Under
    // spec v4.4 the field group is declared once in the header.
    let value = nested_records(1_000);
    let encoded = assert_roundtrip(&value);
    let compact = serde_json::to_string(&value).unwrap();
    assert!(
        encoded.len() < compact.len(),
        "nested TOON {} bytes must beat compact JSON {} bytes",
        encoded.len(),
        compact.len()
    );
}

#[test]
fn keyed_records_encode_smaller_than_compact_json() {
    let value = keyed_records(1_000);
    let encoded = assert_roundtrip(&value);
    let compact = serde_json::to_string(&value).unwrap();
    assert!(
        encoded.len() < compact.len(),
        "keyed TOON {} bytes must beat compact JSON {} bytes",
        encoded.len(),
        compact.len()
    );
}

// --- 0.5.0 encode/decode defects ------------------------------------------

#[test]
fn array_of_empty_objects_round_trips() {
    // 0.5.0 emitted `items[2]{}:`, which its own decoder rejects with
    // `Field list cannot be empty for tabular arrays`.
    let encoded = assert_roundtrip(&json!({ "items": [{}, {}] }));
    assert_eq!(encoded, "items[2]:\n  -\n  -");
}

#[test]
fn single_empty_object_array_round_trips() {
    assert_roundtrip(&json!({ "items": [{}] }));
}

#[test]
fn legacy_empty_field_list_document_is_still_rejected() {
    // Documents the 0.5.0 defect rather than tolerating it: the empty field
    // list stays a decode error, so nothing can silently read `items[2]{}:`
    // as anything other than a failure.
    let error = toon_format::decode_default::<Value>("items[2]{}:")
        .expect_err("empty field list must not decode");
    assert!(
        error.to_string().to_lowercase().contains("field"),
        "unexpected error: {error}"
    );
}

#[test]
fn scientific_notation_stays_numeric() {
    // 0.5.0 decoded `100000000000000000000` back as a string, so the
    // candidate failed the value-equality guard and the payload kept its
    // JSON. 0.7.0 keeps it a number.
    let value: Value = serde_json::from_str(r#"{ "v": 1e20 }"#).unwrap();
    let encoded = assert_roundtrip(&value);
    assert_eq!(encoded, "v: 100000000000000000000");
    assert!(toon_format::decode_default::<Value>(&encoded).expect("decode")["v"].is_number());
}

#[test]
fn unicode_keys_are_quoted_and_round_trip() {
    let encoded = assert_roundtrip(&json!({ "键名": { "子键": 1 } }));
    assert_eq!(encoded, "\"键名\":\n  \"子键\": 1");
}

#[test]
fn dotted_keys_round_trip() {
    let value = json!({ "a.b": 2, "plain": 3 });
    assert_roundtrip(&value);
}

#[test]
fn large_unsigned_integers_stay_exact() {
    let value: Value = serde_json::from_str(r#"{ "v": 18446744073709551615 }"#).unwrap();
    let encoded = assert_roundtrip(&value);
    assert_eq!(encoded, "v: 18446744073709551615");
}

// --- mixed-version expectations -------------------------------------------

/// Documents 0.5.0 actually produced for the shapes above, captured before
/// the upgrade. A rollback leaves text like this in saved `compress-toon`
/// output, so the 0.7.0 decoder must read all of it.
#[test]
fn documents_produced_by_0_5_still_decode() {
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({"users":[{"id":1,"name":"u1","active":true},{"id":2,"name":"u2","active":false}]}),
            "users[2]{id,name,active}:\n  1,u1,true\n  2,u2,false",
        ),
        (
            json!({"users":[{"id":1,"profile":{"name":"u1","active":true}},{"id":2,"profile":{"name":"u2","active":false}}]}),
            "users[2]:\n  - id: 1\n    profile:\n      name: u1\n      active: true\n  - id: 2\n    profile:\n      name: u2\n      active: false",
        ),
        (
            json!({"users":{"u1":{"name":"u1","active":true},"u2":{"name":"u2","active":false}}}),
            "users:\n  u1:\n    name: u1\n    active: true\n  u2:\n    name: u2\n    active: false",
        ),
        (
            json!({"a":[1,2,3],"b":"x","c":null}),
            "a[3]: 1,2,3\nb: x\nc: null",
        ),
        (
            json!({"键名":{"子键":1},"a.b":2}),
            "键名:\n  子键: 1\na.b: 2",
        ),
    ];
    for (index, (expected, document)) in cases.into_iter().enumerate() {
        let decoded = toon_format::decode_default::<Value>(document)
            .unwrap_or_else(|error| panic!("case {index} must decode: {error}"));
        assert_eq!(decoded, expected, "case {index} changed meaning");
    }
}

/// A pre-0.6 decoder rejects exactly these two headers. Pinning them keeps
/// the wire contract that makes a rollback lossy-for-new-text explicit
/// instead of incidental.
#[test]
fn new_headers_use_spec_v44_syntax_only() {
    let nested = nested_record_groups_header();
    assert!(
        nested.contains("]{") && nested.contains("profile{"),
        "nested header lost its field-group syntax: {nested}"
    );
    let keyed = keyed_record_maps_header();
    assert!(
        keyed.contains(":]{"),
        "keyed header lost its tabular-key syntax: {keyed}"
    );
}

fn nested_record_groups_header() -> String {
    assert_roundtrip(&nested_records(2))
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn keyed_record_maps_header() -> String {
    assert_roundtrip(&keyed_records(2))
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

// --- the premise of the JSON-domain guard ---------------------------------

#[test]
fn out_of_domain_numbers_still_lose_their_value() {
    // `toon_candidate` accepts an encoding only when `decode(encode(v)) == v`.
    // These literals break that equality on 0.7.0 exactly as they did on
    // 0.5.0, which is what keeps the JSON fallback in front of them.
    for literal in ["1e-400", "-0.0", "1.0e3"] {
        let value: Value = serde_json::from_str(&format!("{{\"v\":{literal}}}")).unwrap();
        let encoded = toon_format::encode_default(&value).expect("encode");
        let decoded = toon_format::decode_default::<Value>(encoded.trim_end()).expect("decode");
        assert_ne!(
            decoded, value,
            "{literal} must stay outside the TOON numeric domain"
        );
    }
}
