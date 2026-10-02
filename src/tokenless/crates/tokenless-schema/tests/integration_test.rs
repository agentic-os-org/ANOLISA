//! Integration coverage for schema-only compression.

use std::sync::Arc;

use serde_json::{Value, json};
use tokenless_ccr::{InMemoryStore, StashStore};
use tokenless_schema::SchemaCompressor;

const FIXTURES: &[&str] = &[
    "simple_calculator.json",
    "hubspot_contact.json",
    "stripe_payment.json",
    "github_create_issue.json",
    "slack_send_message.json",
    "aws_describe_instances.json",
];

fn load_schema(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/schemas/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to load schema {path}: {error}"));
    serde_json::from_str(&content).unwrap()
}

#[test]
fn simple_schema_keeps_the_function_contract() {
    let schema = json!({
        "function": {
            "name": "greet",
            "description": "Say hello",
            "parameters": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {"type": "string", "enum": ["Ada", "Lin"]}
                }
            }
        }
    });
    let result = SchemaCompressor::new().compress(&schema);
    assert_eq!(result["function"]["name"], "greet");
    assert_eq!(result["function"]["parameters"]["type"], "object");
    assert_eq!(
        result["function"]["parameters"]["required"],
        json!(["name"])
    );
    assert_eq!(
        result["function"]["parameters"]["properties"]["name"]["enum"],
        json!(["Ada", "Lin"])
    );
}

#[test]
fn nested_titles_and_examples_are_removed() {
    let schema = json!({
        "function": {
            "name": "nested",
            "title": "drop",
            "parameters": {
                "type": "object",
                "properties": {
                    "address": {
                        "type": "object",
                        "title": "drop",
                        "properties": {
                            "street": {
                                "type": "string",
                                "title": "drop",
                                "examples": ["Main"]
                            }
                        }
                    }
                }
            }
        }
    });
    let result = SchemaCompressor::new().compress(&schema);
    let street = result
        .pointer("/function/parameters/properties/address/properties/street")
        .unwrap();
    assert!(street.get("title").is_none());
    assert!(street.get("examples").is_none());
    assert!(
        result
            .pointer("/function/parameters/properties/address")
            .unwrap()
            .get("title")
            .is_none()
    );
}

#[test]
fn long_descriptions_are_bounded() {
    let schema = json!({
        "function": {
            "name": "bounded",
            "description": "A".repeat(500),
            "parameters": {"type": "object"}
        }
    });
    let result = SchemaCompressor::new().compress(&schema);
    assert!(result["function"]["description"].as_str().unwrap().len() < 500);
}

#[test]
fn empty_and_scalar_inputs_do_not_panic() {
    let compressor = SchemaCompressor::new();
    assert!(compressor.compress(&json!({})).is_object());
    assert!(compressor.compress(&Value::Null).is_null());
    assert!(compressor.compress(&json!({"function": {}}))["function"].is_object());
}

#[test]
fn every_fixture_round_trips_and_never_grows() {
    let compressor = SchemaCompressor::new();
    for name in FIXTURES {
        let schema = load_schema(name);
        let compressed = compressor.compress(&schema);
        let output = serde_json::to_string(&compressed).unwrap();
        let reparsed: Value = serde_json::from_str(&output).unwrap();
        assert!(reparsed.is_object(), "{name} round-trip failed");
        assert!(
            output.len() <= serde_json::to_string(&schema).unwrap().len(),
            "{name} grew after compression"
        );
        if schema.get("type").is_some() {
            assert_eq!(compressed.get("type"), schema.get("type"));
        }
    }
}

#[test]
fn fixture_compression_saves_at_least_five_percent() {
    let compressor = SchemaCompressor::new();
    let mut before = 0usize;
    let mut after = 0usize;
    for name in FIXTURES {
        let schema = load_schema(name);
        let compressed = compressor.compress(&schema);
        before += serde_json::to_string(&schema).unwrap().len();
        after += serde_json::to_string(&compressed).unwrap().len();
    }
    let saved = (1.0 - after as f64 / before as f64) * 100.0;
    assert!(saved >= 5.0, "expected >= 5% savings, got {saved:.1}%");
}

#[test]
fn stash_backed_description_is_retrievable() {
    let store = Arc::new(InMemoryStore::new());
    let description = "full description ".repeat(80);
    let schema = json!({
        "function": {
            "name": "retrieve",
            "description": description,
            "parameters": {"type": "object"}
        }
    });
    let compressed = SchemaCompressor::new()
        .with_stash_store(store.clone())
        .compress(&schema);
    let marker = compressed["function"]["description"].as_str().unwrap();
    let hash = tokenless_ccr::extract_hash(marker).unwrap();
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(description.as_str())
    );
}
