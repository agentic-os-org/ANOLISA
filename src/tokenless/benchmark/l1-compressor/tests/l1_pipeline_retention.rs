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

//! End-to-end pipeline retention tests.
//!
//! Verifies that canonical payloads traversing the compression pipeline
//! (JsonCompressor/SchemaCompressor → TOON encode → TOON decode) retain
//! their semantic fields while noise is stripped.
//!
//! Decode outcomes are pinned, never swallowed: the pipeline helper returns
//! the raw `Result` of a STRICT TOON decode — the exact mode the paired
//! `tokenless decompress-toon` command uses — and every test asserts decode
//! success for its configuration. All truncation-marker shapes round-trip:
//! the plain marker carries a trailing `, not stashed` clause and the stash
//! marker carries the retrieval key, both of which force the TOON encoder to
//! quote the string, so the marker survives the round-trip intact whether it
//! sits between the head and tail items (default config), after the last
//! kept item (head-only config), or in a stash-protected array. A regression
//! that makes any supported shape undecodable fails loudly here instead of
//! shipping output the paired decoder rejects.

use std::sync::Arc;

use serde_json::{Value, json};
use tokenless_bench::{compress_json, compress_json_with, response_canonical, schema_canonical};
use tokenless_ccr::{InMemoryStore, StashStore, extract_hash, recovery_instruction};
use tokenless_compressors::{JsonCompressionConfig, RecoveryMethod};
use tokenless_schema::SchemaCompressor;

/// Compress a response value (stage 1 of the pipeline).
fn response_compressed(value: &Value) -> Value {
    compress_json(value)
}

/// Run a response value through compress → TOON encode → TOON decode and
/// return the raw decode outcome.
///
/// Decodes in strict mode because that is what the paired decoder
/// (`tokenless decompress-toon`) does: output this pipeline accepts must be
/// acceptable to it. The outcome is returned to the caller instead of being
/// swallowed — every test pins decode success for a supported configuration,
/// so a regression that produces undecodable TOON fails loudly.
fn response_pipeline(
    value: &Value,
    config: JsonCompressionConfig,
    stash: Option<&dyn StashStore>,
) -> Result<Value, toon_format::ToonError> {
    let (compressed, _) = compress_json_with(value, config, stash);
    let encoded = toon_format::encode_default(&compressed).expect("TOON encode");
    toon_format::decode_default::<Value>(&encoded)
}

/// Run a schema value through compress → TOON encode → TOON decode.
fn schema_pipeline(value: &Value) -> Value {
    let compressed = SchemaCompressor::new().compress(value);
    let encoded = toon_format::encode_default(&compressed).expect("TOON encode");
    toon_format::decode_default::<Value>(&encoded).expect("TOON decode")
}

fn non_record_array_response() -> Value {
    json!({
        "tool": "search_code",
        "status": "ok",
        "results": (0..60)
            .map(|index| format!("result-{index}-{}", "x".repeat(80)))
            .collect::<Vec<_>>()
    })
}

#[test]
fn response_pipeline_preserves_tool_and_status() {
    // Tool and status are top-level scalar keys appearing after the large
    // `results` list. With the TOON-safe truncation marker the strict
    // round-trip recovers them, so assert on real decoded output.
    let decoded = response_pipeline(
        &response_canonical(),
        JsonCompressionConfig::default(),
        None,
    )
    .expect("default no-stash pipeline must round-trip through strict TOON decode");
    assert_eq!(decoded["tool"], "search_code");
    assert_eq!(decoded["status"], "ok");
    // The compression stage must preserve them too.
    let compressed = response_compressed(&response_canonical());
    assert_eq!(compressed["tool"], "search_code");
    assert_eq!(compressed["status"], "ok");
}

#[test]
fn response_pipeline_preserves_result_item_fields() {
    // Lossless cleanup exceeds the selection threshold for the canonical
    // response, so all records survive rather than entering bounded reduction.
    let decoded = response_pipeline(
        &response_canonical(),
        JsonCompressionConfig::default(),
        None,
    )
    .expect("default no-stash pipeline must round-trip through strict TOON decode");
    let results = decoded["results"]
        .as_array()
        .expect("results array exists after pipeline");
    assert_eq!(results.len(), 60, "lossless cleanup keeps every record");
    let first = &results[0];
    assert!(first["id"].is_number(), "id preserved");
    assert!(first["name"].is_string(), "name preserved");
    assert!(first["path"].is_string(), "path preserved");
    assert!(first["status"].is_string(), "status preserved");
    assert!(first["score"].is_number(), "score preserved");
    assert!(results.iter().all(Value::is_object));
}

#[test]
fn response_pipeline_drops_noise_fields() {
    let decoded = response_pipeline(
        &non_record_array_response(),
        JsonCompressionConfig::default(),
        None,
    )
    .expect("default no-stash pipeline must round-trip through strict TOON decode");
    let obj = decoded.as_object().expect("decoded response is an object");
    // Top-level noise fields dropped by the compressor.
    for k in ["debug", "trace", "logs"] {
        assert!(
            !obj.contains_key(k),
            "{k} should be dropped by the pipeline"
        );
    }
    // Per-item debug field also stripped from kept result entries.
    if let Some(results) = decoded["results"].as_array() {
        for item in results.iter().take(5) {
            if item.is_object() {
                assert!(
                    item.get("debug").is_none(),
                    "debug should be dropped from result items"
                );
            }
        }
    }
}

#[test]
fn response_pipeline_head_only_marker_roundtrips() {
    // Head-only truncation (array_tail_preserve = 0) appends the plain
    // marker after the last kept item; the quoted marker round-trips and
    // the root-level keys after the array are recovered as well.
    let decoded = response_pipeline(
        &non_record_array_response(),
        JsonCompressionConfig {
            array_tail_preserve: 0,
            ..JsonCompressionConfig::default()
        },
        None,
    )
    .expect("head-only truncation must round-trip through strict TOON decode");
    let results = decoded["results"]
        .as_array()
        .expect("results array exists after pipeline");
    // 32 head + 1 trailing marker = 33 items.
    assert_eq!(results.len(), 33, "32 head items + trailing marker");
    assert!(
        results
            .last()
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("more items truncated, not stashed")),
        "trailing marker survives the round-trip intact"
    );
    assert_eq!(decoded["tool"], "search_code", "root keys recovered");
    assert_eq!(decoded["status"], "ok", "root keys recovered");
}

#[test]
fn response_pipeline_stash_marker_roundtrips_intact() {
    // With a stash store attached, the marker carries the retrieval key and
    // the TOON encoder quotes it, so the full combined pipeline round-trips:
    // decode succeeds, the marker text (including the stash key) survives
    // intact, and the root-level keys after the array are recovered too.
    // This is the reversible-production shape; pin it end to end.
    let store = Arc::new(InMemoryStore::new());
    let decoded = response_pipeline(
        &non_record_array_response(),
        JsonCompressionConfig::default(),
        Some(store.as_ref()),
    )
    .expect("stash-marker shape must round-trip through strict TOON decode");
    let results = decoded["results"]
        .as_array()
        .expect("results array exists after pipeline");
    // 32 head + 1 marker + 8 tail = 41 items.
    assert_eq!(results.len(), 41, "head + marker + tail preserved");
    let marker = results[32]
        .as_str()
        .expect("marker sits between head and tail");
    let hash = extract_hash(marker)
        .unwrap_or_else(|| panic!("stash key survives the round-trip intact: {marker}"));
    assert!(
        marker.ends_with(&recovery_instruction(hash, &RecoveryMethod::Shell)),
        "new instruction form, not a legacy marker: {marker}"
    );
    assert!(
        store.retrieve(hash).expect("stash readable").is_some(),
        "recovered key resolves to the stashed payload"
    );
    assert_eq!(store.len(), 1, "one stash entry for the dropped middle");
    assert_eq!(decoded["tool"], "search_code", "root keys recovered");
    assert_eq!(decoded["status"], "ok", "root keys recovered");
}

#[test]
fn schema_pipeline_preserves_function_name_and_properties() {
    let decoded = schema_pipeline(&schema_canonical());
    assert_eq!(decoded["function"]["name"], "search_code");
    assert!(
        decoded["function"]["parameters"]["properties"].is_object(),
        "properties preserved"
    );
    assert_eq!(
        decoded["function"]["parameters"]["type"], "object",
        "type preserved"
    );
}

#[test]
fn schema_pipeline_preserves_semantic_fields() {
    // The canonical schema does not carry required/enum/default/const, so use
    // a synthetic schema that does — same pattern as schema_retention.rs.
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
    let decoded = schema_pipeline(&schema);
    assert_eq!(decoded["function"]["name"], "my_function");
    let params = &decoded["function"]["parameters"];
    assert_eq!(params["type"], "object");
    assert!(params["required"].is_array());
    let f1 = &params["properties"]["field1"];
    assert!(f1["enum"].is_array());
    assert_eq!(f1["default"], "a");
    assert_eq!(f1["const"], "fixed");
}
