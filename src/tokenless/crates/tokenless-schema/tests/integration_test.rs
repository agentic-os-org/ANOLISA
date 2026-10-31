//! Integration coverage for schema-only compression.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};
use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore, recovery_hashes};
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

#[test]
fn referenced_keys_do_not_grant_rollback_ownership() {
    let store = Arc::new(InMemoryStore::default());
    let original = "Preserve this original description for existing callers. ".repeat(100);
    let existing = store.stash(&original).unwrap();
    let compressor = SchemaCompressor::new().with_stash_store(store.clone());
    let schema = json!({"function":{"name":"read","description":original}});
    let first = compressor.compress(&schema);
    assert_eq!(first, compressor.compress(&schema));
    assert_eq!(
        compressor.referenced_stash_keys(),
        vec![existing.key.clone()]
    );
    assert!(compressor.stash_keys().is_empty());
    assert_eq!(compressor.rollback_stash_writes(), 0);
    assert!(compressor.referenced_stash_keys().is_empty());
    assert_eq!(store.retrieve(&existing.key).unwrap(), Some(original));
}

#[test]
fn references_reset_independently_of_stored_rows() {
    let store = Arc::new(InMemoryStore::default());
    let description = "Long source content used by the schema compressor. ".repeat(100);
    let schema = json!({"function":{"name":"read","description":description}});
    let compressor = SchemaCompressor::new().with_stash_store(store.clone());
    compressor.compress(&schema);
    let keys = compressor.referenced_stash_keys();
    assert_eq!(keys.len(), 1);
    let mut owned_keys = compressor.stash_keys();
    owned_keys.sort();
    assert_eq!(owned_keys, keys);
    compressor.clear_stash_session();
    assert!(compressor.referenced_stash_keys().is_empty());
    assert_eq!(store.retrieve(&keys[0]).unwrap(), Some(description));
    compressor.compress(&schema);
    assert_eq!(compressor.referenced_stash_keys(), keys);
    assert!(compressor.stash_keys().is_empty());
    assert_eq!(compressor.rollback_stash_writes(), 0);
}

#[test]
fn mixed_foreign_refresh_references_do_not_transfer_rollback_ownership() {
    let store = Arc::new(InMemoryStore::default());
    let first = SchemaCompressor::new().with_stash_store(store.clone());
    let other = SchemaCompressor::new().with_stash_store(store.clone());
    let original = "Existing source description with enough detail. ".repeat(100);
    let fresh = "Different source created during this candidate session. ".repeat(100);
    let schema = json!({"function": {"name": "read", "description": original}});

    let initial_output = first.compress(&schema);
    other.compress(&schema);
    let refreshed_output = first.compress(&schema);
    assert_eq!(initial_output, refreshed_output);
    assert!(
        first.stash_keys().is_empty(),
        "foreign refresh ends ownership"
    );
    let fresh_output = first.compress(&json!({
        "function": {"name": "write", "description": fresh}
    }));
    let output = serde_json::to_string(&json!([refreshed_output, fresh_output])).unwrap();
    let visible = recovery_hashes(&output, &RecoveryMethod::Shell)
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let references = first.referenced_stash_keys();
    assert_eq!(
        references, visible,
        "references match the candidate markers"
    );
    assert_eq!(references.len(), 2);
    let owned = first.stash_keys();
    assert_eq!(owned.len(), 1);
    assert_eq!(
        store.retrieve(&owned[0]).unwrap().as_deref(),
        Some(fresh.as_str())
    );
    let foreign_key = references.iter().find(|key| !owned.contains(key)).unwrap();

    assert_eq!(first.rollback_stash_writes(), 1);
    assert!(first.referenced_stash_keys().is_empty());
    assert_eq!(references, visible, "the saved snapshot survives clearing");
    assert_eq!(store.retrieve(&owned[0]).unwrap(), None);
    assert_eq!(
        store.retrieve(foreign_key).unwrap().as_deref(),
        Some(original.as_str())
    );
    assert_eq!(other.rollback_stash_writes(), 0);
    assert!(other.referenced_stash_keys().is_empty());
    assert_eq!(
        store.retrieve(foreign_key).unwrap().as_deref(),
        Some(original.as_str())
    );
}
