use std::sync::Arc;

use serde_json::Value;
use tokenless_ccr::{InMemoryStore, StashError, StashStore, StashWrite, extract_hash};

fn context<'a>(stash: Option<&'a dyn StashStore>) -> JsonCompressionContext<'a> {
    JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        stash,
        allow_toon: false,
        min_toon_chars: 500,
        allow_unrecoverable: true,
    }
}

/// Whether `output` carries `literal` as a complete numeric token: unquoted,
/// unrounded, and not merely a prefix of a longer number.
fn retains_number(output: &str, literal: &str) -> bool {
    output
        .match_indices(&format!("\"value\":{literal}"))
        .any(|(start, matched)| {
            matches!(
                output[start + matched.len()..].chars().next(),
                Some(',') | Some('}') | Some(']')
            )
        })
}

fn output_value(outcome: &JsonOutcome) -> Value {
    serde_json::from_str(&outcome.output).expect("JSON representation")
}

#[test]
fn recovery_instruction_is_budgeted_for_each_method() {
    use tokenless_ccr::{RecoveryMethod, recovery_hashes, truncation_suffix_for};
    for method in [
        RecoveryMethod::Shell,
        RecoveryMethod::tool("t".repeat(64)).unwrap(),
    ] {
        let store = InMemoryStore::new();
        let suffix = truncation_suffix_for("0123456789abcdef01234567", &method)
            .chars()
            .count();
        for budget in [suffix - 1, suffix, suffix + 1] {
            let input = serde_json::to_string(&"世界".repeat(500)).unwrap();
            let outcome = JsonCompressor::new(JsonCompressionConfig {
                truncate_strings_at: budget,
                ..JsonCompressionConfig::default()
            })
            .compress(
                &input,
                &JsonCompressionContext {
                    recovery: &method,
                    ..context(Some(&store))
                },
            )
            .unwrap();
            let value = output_value(&outcome);
            let text = value.as_str().unwrap();
            assert!(text.chars().count() <= budget);
            let hashes = recovery_hashes(&outcome.output, &method);
            assert_eq!(hashes.len(), usize::from(budget > suffix));
            for hash in hashes {
                assert_eq!(store.retrieve(hash).unwrap().unwrap(), "世界".repeat(500));
            }
            assert!(!outcome.output.contains("<<tokenless:"));
        }
    }
}

#[test]
fn every_json_omission_uses_the_declared_static_tool() {
    use tokenless_ccr::{RecoveryMethod, recovery_hashes};
    let method = RecoveryMethod::tool("tenant_retrieve").unwrap();
    let cases = [
        (
            serde_json::json!({"value": "word ".repeat(1000)}),
            JsonCompressionConfig {
                truncate_strings_at: 180,
                ..JsonCompressionConfig::default()
            },
        ),
        (
            serde_json::json!({"value": {"nested": "word ".repeat(1000)}}),
            JsonCompressionConfig {
                max_depth: 0,
                ..JsonCompressionConfig::default()
            },
        ),
        (
            serde_json::json!(
                (0..100)
                    .map(|n| format!("{n}{}", "x".repeat(100)))
                    .collect::<Vec<_>>()
            ),
            JsonCompressionConfig {
                truncate_arrays_at: 4,
                ..JsonCompressionConfig::default()
            },
        ),
        (
            serde_json::json!(
                records(100)
                    .into_iter()
                    .map(|mut record| {
                        record["message"] = serde_json::json!("ordinary content ".repeat(60));
                        record
                    })
                    .collect::<Vec<_>>()
            ),
            JsonCompressionConfig::default(),
        ),
    ];
    for (index, (value, config)) in cases.into_iter().enumerate() {
        for allow_toon in [false, true] {
            let store = InMemoryStore::new();
            let outcome = JsonCompressor::new(config.clone())
                .compress(
                    &value.to_string(),
                    &JsonCompressionContext {
                        recovery: &method,
                        allow_toon,
                        min_toon_chars: 0,
                        ..context(Some(&store))
                    },
                )
                .unwrap();
            assert_eq!(
                outcome.recoverability,
                Recoverability::Retrievable,
                "case={index} toon={allow_toon}"
            );
            assert!(!outcome.output.contains("<<tokenless:"));
            assert!(!outcome.output.contains("run in shell"));
            let hashes = recovery_hashes(&outcome.output, &method);
            assert!(!hashes.is_empty(), "{}", outcome.output);
            for hash in hashes {
                assert!(store.retrieve(hash).unwrap().is_some());
            }
        }
    }
}

/// Parses JSON text into the exact document the compressor works on.
fn parsed(text: &str) -> ExactValue<'_> {
    exact::parse(text).expect("valid JSON document")
}

/// Object members of a parsed document.
fn members<'a>(document: &'a ExactValue<'a>) -> &'a ExactMap<'a> {
    document.as_object().expect("object document")
}

/// Array items of a parsed document.
fn items<'a>(document: &'a ExactValue<'a>) -> &'a [ExactValue<'a>] {
    match document {
        ExactValue::Array(items) => items,
        _ => panic!("array document"),
    }
}

fn records(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            serde_json::json!({
                "id": index,
                "message": format!("record-{index}-{}", "x".repeat(80)),
                "status": "ok"
            })
        })
        .collect()
}

fn selected_record_ids(outcome: &JsonOutcome) -> Vec<u64> {
    output_value(outcome)
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.get("id").and_then(Value::as_u64))
        .collect()
}

#[test]
fn compaction_preserves_fields_without_recovery() {
    let input = "{\n  \"data\": \"kept\",\n  \"debug\": \"evidence\",\n  \"empty\": null\n}";
    let outcome = JsonCompressor::default()
        .compress(input, &context(None))
        .unwrap();
    assert_eq!(
        output_value(&outcome),
        serde_json::from_str::<Value>(input).unwrap()
    );
    assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
    assert!(outcome.stash_writes.is_empty());
}

#[test]
fn string_truncation_is_unicode_safe_and_bounded() {
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_strings_at: 10,
        ..JsonCompressionConfig::default()
    });
    let outcome = compressor
        .compress(
            r#"{"value":"你好世界，这是一个很长的测试"}"#,
            &context(None),
        )
        .unwrap();
    let value = output_value(&outcome);
    assert!(value["value"].as_str().unwrap().chars().count() <= 10);
    assert_eq!(
        outcome.operations,
        [JsonOperation::Truncation],
        "truncation is not mislabeled as cleanup"
    );
    assert_eq!(outcome.recoverability, Recoverability::Unrecoverable);
    assert_eq!(outcome.metrics.truncations, 1);
    assert_eq!(outcome.metrics.unrecoverable_truncations, 1);
}

#[test]
fn array_truncation_preserves_head_and_tail() {
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_arrays_at: 3,
        array_tail_preserve: 2,
        ..JsonCompressionConfig::default()
    });
    let input = serde_json::to_string(
        &(1..=10)
            .map(|index| format!("item-{index}-{}", "x".repeat(80)))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let outcome = compressor.compress(&input, &context(None)).unwrap();
    let output = output_value(&outcome);
    let array = output.as_array().unwrap();
    assert!(array[0].as_str().unwrap().starts_with("item-1-"));
    assert!(array[2].as_str().unwrap().starts_with("item-3-"));
    assert!(
        array[array.len() - 2]
            .as_str()
            .unwrap()
            .starts_with("item-9-")
    );
    assert!(
        array[array.len() - 1]
            .as_str()
            .unwrap()
            .starts_with("item-10-")
    );
    assert!(array[3].as_str().unwrap().contains("5 more items"));
    assert_eq!(outcome.recoverability, Recoverability::Unrecoverable);
    assert_eq!(outcome.metrics.unrecoverable_truncations, 1);
}

#[test]
fn complete_candidates_preserve_empty_values_fields_and_array_positions() {
    let value = serde_json::json!({
        "debug": {"cause": "connection refused"}, "trace": "request trace",
        "traces": [], "stack": "frame", "stacktrace": "location", "logs": ["failure"],
        "logging": false, "null": null, "empty_string": "", "empty_array": [], "empty_object": {},
        "nested": {"debug": null, "empty": {}, "array": [10, null, 20, "", [], {}, false, 0, null]},
        "duplicates": [{"value": null}, {"value": null}]
    });
    for input in [
        value.to_string(),
        serde_json::to_string_pretty(&value).unwrap(),
    ] {
        for allow_toon in [false, true] {
            let store = InMemoryStore::new();
            let outcome = JsonCompressor::default()
                .compress(
                    &input,
                    &JsonCompressionContext {
                        allow_toon,
                        min_toon_chars: 0,
                        ..context(Some(&store))
                    },
                )
                .unwrap();
            let decoded = if outcome.operations.contains(&JsonOperation::Toon) {
                toon_format::decode_default::<Value>(&outcome.output).unwrap()
            } else {
                output_value(&outcome)
            };
            assert_eq!(decoded, value, "toon={allow_toon}");
            assert_eq!(outcome.recoverability, Recoverability::Lossless);
            assert_eq!(outcome.metrics, JsonMetrics::default());
            assert!(outcome.stash_writes.is_empty());
            assert!(store.is_empty());
        }
    }
}

#[test]
fn string_bounds_preserve_sibling_and_nested_empty_fields() {
    let input = serde_json::json!({
        "body": "word ".repeat(1200),
        "debug": "evidence", "trace": null, "logs": [],
        "nested": {"stack": "frame", "empty": {}, "items": [null, "", [], {}, false, 0]}
    });
    for attached in [false, true] {
        let store = InMemoryStore::new();
        let outcome = JsonCompressor::default()
            .compress(
                &input.to_string(),
                &context(attached.then_some(&store as &dyn StashStore)),
            )
            .unwrap();
        assert_eq!(outcome.operations, [JsonOperation::Truncation]);
        let mut output = output_value(&outcome);
        assert!(output["body"].as_str().unwrap().len() < input["body"].as_str().unwrap().len());
        output["body"] = input["body"].clone();
        assert_eq!(output, input);
        assert_eq!(
            outcome.recoverability,
            if attached {
                Recoverability::Retrievable
            } else {
                Recoverability::Unrecoverable
            }
        );
        if attached {
            let key = extract_hash(&outcome.output).unwrap();
            assert_eq!(
                store.retrieve(key).unwrap().as_deref(),
                input["body"].as_str()
            );
        }
    }
}

#[test]
fn array_bounds_keep_empty_head_and_tail_positions() {
    let mut values = vec![
        Value::Null,
        serde_json::json!(""),
        serde_json::json!([]),
        serde_json::json!({}),
    ];
    let dropped = vec![
        serde_json::json!("evidence ".repeat(200)),
        Value::Null,
        serde_json::json!({"logs": []}),
    ];
    values.extend(dropped.clone());
    values.extend([
        serde_json::json!({}),
        serde_json::json!([]),
        serde_json::json!(""),
        Value::Null,
    ]);
    let store = InMemoryStore::new();
    let outcome = JsonCompressor::new(JsonCompressionConfig {
        truncate_arrays_at: 4,
        array_tail_preserve: 4,
        ..Default::default()
    })
    .compress(
        &serde_json::to_string(&values).unwrap(),
        &context(Some(&store)),
    )
    .unwrap();
    let output = output_value(&outcome);
    let output = output.as_array().unwrap();
    assert_eq!(&output[..4], &values[..4]);
    assert_eq!(&output[5..], &values[values.len() - 4..]);
    assert_eq!(output.len(), 9);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    let key = extract_hash(output[4].as_str().unwrap()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&store.retrieve(key).unwrap().unwrap()).unwrap(),
        serde_json::json!(dropped)
    );
}

#[test]
fn depth_truncation_stashes_the_exact_subtree() {
    let store = InMemoryStore::new();
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        max_depth: 0,
        ..JsonCompressionConfig::default()
    });
    let subtree = serde_json::json!({"value": "exact payload".repeat(40)});
    let input = serde_json::to_string(&serde_json::json!({"nested": subtree})).unwrap();
    let outcome = compressor.compress(&input, &context(Some(&store))).unwrap();
    assert_eq!(outcome.operations, [JsonOperation::Truncation]);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    let hash = extract_hash(&outcome.output).unwrap();
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(serde_json::to_string(&subtree).unwrap().as_str())
    );
}

#[test]
fn max_depth_keeps_scalar_leaves() {
    let store = InMemoryStore::new();
    let outcome = JsonCompressor::new(JsonCompressionConfig {
        max_depth: 1,
        ..JsonCompressionConfig::default()
    })
    .compress(
        &serde_json::json!({
            "big": {"deep": {"payload": "x".repeat(600)}},
            "meta": {"keep": 1}
        })
        .to_string(),
        &context(Some(&store)),
    )
    .unwrap();
    let value = output_value(&outcome);
    assert!(value["big"]["deep"]
        .as_str()
        .unwrap()
        .starts_with("object truncated at depth 2"));
    assert_eq!(value["meta"]["keep"], 1);
}

#[test]
fn scalar_leaves_past_max_depth_are_neither_stashed_nor_counted() {
    use tokenless_ccr::compute_key;

    let store = InMemoryStore::new();
    let items = serde_json::json!(["a".repeat(300), "b".repeat(300), "c".repeat(300)]);
    let input = serde_json::to_string(&serde_json::json!({
        "meta": {"count": 1, "flag": false, "missing": null, "items": items}
    }))
    .unwrap();
    let outcome = JsonCompressor::new(JsonCompressionConfig {
        max_depth: 1,
        ..JsonCompressionConfig::default()
    })
    .compress(&input, &context(Some(&store)))
    .unwrap();

    let value = output_value(&outcome);
    assert!(value["meta"]["count"].is_number());
    assert_eq!(value["meta"]["count"], serde_json::json!(1));
    assert_eq!(value["meta"]["flag"], serde_json::json!(false));
    assert!(value["meta"]["missing"].is_null());
    // Containers can still nest past the limit, so they keep their marker.
    assert!(value["meta"]["items"]
        .as_str()
        .unwrap()
        .starts_with("array truncated at depth 2"));

    // The array is the only bounded value: one truncation and one Stash row,
    // holding the array rather than a scalar leaf.
    assert_eq!(outcome.metrics.truncations, 1);
    assert_eq!(outcome.metrics.unrecoverable_truncations, 0);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert_eq!(store.len(), 1);
    let hash = extract_hash(&outcome.output).unwrap();
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(serde_json::to_string(&items).unwrap().as_str())
    );
    for payload in ["1", "false", "null"] {
        assert!(
            store
                .retrieve(&compute_key(payload.as_bytes()))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn depth_bounding_that_only_meets_scalars_is_lossless() {
    let store = InMemoryStore::new();
    let input = r#"{"meta":{"count":1,"flag":false,"missing":null}}"#;
    let outcome = JsonCompressor::new(JsonCompressionConfig {
        max_depth: 1,
        ..JsonCompressionConfig::default()
    })
    .compress(input, &context(Some(&store)))
    .unwrap();

    assert_eq!(outcome.output, input);
    assert_eq!(outcome.metrics.truncations, 0);
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn stashed_array_tail_round_trips_exactly() {
    let store = InMemoryStore::new();
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_arrays_at: 3,
        array_tail_preserve: 0,
        ..JsonCompressionConfig::default()
    });
    let values = (1..=8)
        .map(|index| format!("item-{index}-{}", "x".repeat(80)))
        .collect::<Vec<_>>();
    let input = serde_json::to_string(&values).unwrap();
    let outcome = compressor.compress(&input, &context(Some(&store))).unwrap();
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.stash_writes.len(), 1);
    let hash = extract_hash(&outcome.output).unwrap();
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(serde_json::to_string(&values[3..]).unwrap().as_str())
    );
}

#[test]
fn compact_structured_slots_preserve_all_fields_without_an_operation() {
    let input =
        r#"{"stdout":"value","stderr":"","debug":"evidence","nested":{"logs":[],"note":null}}"#;
    let outcome = JsonCompressor::default()
        .compress(input, &context(None))
        .unwrap();
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
}

#[test]
fn json_string_envelope_is_normalized_once() {
    let input =
        serde_json::to_string(r#"{"data":"kept","debug":"evidence","empty":null}"#).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(None))
        .unwrap();
    assert_eq!(
        outcome.output,
        r#"{"data":"kept","debug":"evidence","empty":null}"#
    );
    assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
    assert!(outcome.stash_writes.is_empty());
}

#[test]
fn envelope_savings_prevent_unnecessary_record_reduction() {
    let original = serde_json::json!(
        (0..64)
            .map(|id| serde_json::json!({"id": id, "name": "value", "empty": null}))
            .collect::<Vec<_>>()
    );
    let inner = original.to_string();
    let input = serde_json::to_string(&inner).unwrap();
    assert!(saves_at_least_percent(&inner, &input, 15));
    let store = InMemoryStore::new();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&store)))
        .unwrap();
    assert_eq!(output_value(&outcome), original);
    assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
    assert_eq!(outcome.metrics, JsonMetrics::default());
    assert!(outcome.stash_writes.is_empty());
}

#[test]
fn strings_that_do_not_directly_wrap_containers_are_unchanged() {
    let nested = serde_json::to_string(r#"{"data":"kept"}"#).unwrap();
    for inner in ["42", "null", "ordinary text", &nested] {
        let input = serde_json::to_string(inner).unwrap();
        let outcome = JsonCompressor::default()
            .compress(&input, &context(None))
            .unwrap();
        assert_eq!(outcome.output, input);
        assert!(outcome.operations.is_empty());
    }
}

#[test]
fn invalid_json_is_an_error() {
    assert!(
        JsonCompressor::default()
            .compress("not json", &context(None))
            .is_err()
    );
}

#[test]
fn json_scalar_is_a_no_op() {
    let outcome = JsonCompressor::default()
        .compress("42", &context(None))
        .unwrap();
    assert_eq!(outcome.output, "42");
    assert!(outcome.operations.is_empty());
}

#[test]
fn toon_is_an_internal_json_operation() {
    let input = format!(
        r#"{{"items":[{}]}}"#,
        (0..80)
            .map(|index| format!(r#"{{"id":{index},"name":"item-{index}"}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let context = JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        allow_toon: true,
        min_toon_chars: 0,
        ..context(None)
    };
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_arrays_at: 200,
        ..JsonCompressionConfig::default()
    });
    let outcome = compressor.compress(&input, &context).unwrap();
    assert_eq!(outcome.operations.last(), Some(&JsonOperation::Toon));
    assert!(serde_json::from_str::<Value>(&outcome.output).is_err());
}

struct AlwaysFail;

impl StashStore for AlwaysFail {
    fn stash(&self, _payload: &str) -> Result<StashWrite, StashError> {
        Err(StashError::Backend("simulated".to_owned()))
    }

    fn retrieve(&self, _hash: &str) -> Result<Option<String>, StashError> {
        Ok(None)
    }

    fn len(&self) -> usize {
        0
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        Ok(0)
    }

    fn delete(&self, _hash: &str, _generation: u64) -> Result<bool, StashError> {
        Ok(false)
    }
}

#[test]
fn stash_failure_is_visible_and_degrades_recovery() {
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_strings_at: tokenless_ccr::truncation_suffix_char_len() + 16,
        ..JsonCompressionConfig::default()
    });
    let outcome = compressor
        .compress(
            &serde_json::to_string(&"x".repeat(200)).unwrap(),
            &context(Some(&AlwaysFail)),
        )
        .unwrap();
    assert_eq!(outcome.metrics.stash_errors, 1);
    assert_eq!(outcome.metrics.unrecoverable_truncations, 1);
    assert_eq!(outcome.recoverability, Recoverability::Unrecoverable);
}

#[test]
fn duplicate_stash_payloads_return_every_write_for_the_runtime_ledger() {
    let store = Arc::new(InMemoryStore::new());
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_arrays_at: 2,
        array_tail_preserve: 0,
        ..JsonCompressionConfig::default()
    });
    let outcome = compressor
        .compress(
            r#"{"a":[1,2,3,4,5],"b":[1,2,3,4,5]}"#,
            &context(Some(store.as_ref())),
        )
        .unwrap();
    assert_eq!(outcome.stash_writes.len(), 2);
    assert_eq!(store.len(), 1);
}

#[test]
fn lossless_threshold_is_inclusive_at_fifteen_percent() {
    assert!(saves_at_least_percent(&"x".repeat(68), &"x".repeat(80), 15));
    assert!(!saves_at_least_percent(
        &"x".repeat(69),
        &"x".repeat(80),
        15
    ));
}

#[test]
fn record_array_requires_thirty_three_objects() {
    let small = serde_json::to_string(&records(32)).unwrap();
    assert!(!is_record_array(items(&parsed(&small))));
    let large = serde_json::to_string(&records(33)).unwrap();
    assert!(is_record_array(items(&parsed(&large))));
    let mut mixed = records(33);
    mixed[16] = Value::Null;
    let mixed = serde_json::to_string(&mixed).unwrap();
    assert!(!is_record_array(items(&parsed(&mixed))));
}

#[test]
fn error_signals_require_nonempty_fields_or_matching_status_text() {
    assert!(!has_error_signal(members(&parsed(
        r#"{"error": null, "status": "ok"}"#
    ))));
    assert!(has_error_signal(members(&parsed(
        r#"{"errors": ["boom"]}"#
    ))));
    assert!(has_error_signal(members(&parsed(
        r#"{"severity": "Critical warning"}"#
    ))));
}

#[test]
fn record_reduction_stashes_the_complete_array_with_a_fixed_budget() {
    let store = InMemoryStore::new();
    let values = records(40);
    let input = serde_json::to_string(&values).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&store)))
        .unwrap();

    let ids = selected_record_ids(&outcome);
    assert_eq!(ids.len(), RECORD_MAX_ITEMS);
    assert_eq!(&ids[..4], &[0, 1, 2, 3]);
    assert_eq!(&ids[ids.len() - 4..], &[36, 37, 38, 39]);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(ids.windows(2).all(|pair| pair[1] - pair[0] <= 2));
    assert_eq!(
        ids,
        vec![
            0, 1, 2, 3, 4, 6, 7, 8, 10, 11, 12, 14, 15, 16, 18, 19, 20, 22, 23, 24, 26, 27, 28, 30,
            31, 32, 34, 35, 36, 37, 38, 39,
        ]
    );
    assert_eq!(outcome.operations, [JsonOperation::RecordReduction]);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.metrics.record_reductions, 1);
    assert_eq!(outcome.metrics.records_omitted, 8);
    assert_eq!(outcome.metrics.unrecoverable_truncations, 0);
    assert_eq!(outcome.stash_writes.len(), 1);

    let hash = extract_hash(&outcome.output).unwrap();
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(input.as_str())
    );
}

#[test]
fn record_array_without_stash_does_not_fall_back_to_positional_truncation() {
    let values = records(40);
    let input = serde_json::to_string(&values).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(None))
        .unwrap();

    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(outcome.metrics.record_reductions, 0);
    assert_eq!(outcome.metrics.truncations, 0);
}

#[test]
fn record_stash_failure_keeps_every_record() {
    let values = records(40);
    let input = serde_json::to_string(&values).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&AlwaysFail)))
        .unwrap();

    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(outcome.metrics.stash_errors, 1);
    assert_eq!(outcome.metrics.record_reductions, 0);
}

#[test]
fn record_reduction_preserves_selected_empty_records_and_recovery() {
    let store = InMemoryStore::new();
    let mut values = records(40);
    values[0] = serde_json::json!({ "note": null, "debug": "evidence", "logs": [], "empty": "" });
    values[1] = serde_json::json!({});
    let input = serde_json::to_string(&values).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&store)))
        .unwrap();

    let array = output_value(&outcome);
    let array = array.as_array().unwrap();
    let kept = array.iter().filter(|value| value.is_object()).count();
    assert_eq!(kept, RECORD_MAX_ITEMS);
    assert_eq!(array[0], values[0]);
    assert_eq!(array[1], values[1]);
    assert_eq!(outcome.operations, [JsonOperation::RecordReduction]);
    assert_eq!(outcome.metrics.record_reductions, 1);
    assert_eq!(outcome.metrics.records_omitted, values.len() - kept);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);

    let hash = extract_hash(&outcome.output).unwrap();
    let expected_marker = format!(
        "{} of {} records omitted. {}",
        values.len() - kept,
        values.len(),
        recovery_instruction(hash, &RecoveryMethod::Shell)
    );
    assert_eq!(
        array.last().and_then(Value::as_str),
        Some(expected_marker.as_str())
    );
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(input.as_str())
    );
}

#[test]
fn record_selection_preserves_error_and_structural_anomalies() {
    let mut values = records(100);
    let baseline_text = serde_json::to_string(&values).unwrap();
    let baseline = select_record_indices(items(&parsed(&baseline_text)));
    let error_index = (RECORD_HEAD_ITEMS..values.len() - RECORD_TAIL_ITEMS)
        .find(|index| !baseline.contains(index))
        .unwrap();
    values[error_index]["status"] = Value::String("WARNING".to_owned());
    let structural_index = (error_index + 1..values.len() - RECORD_TAIL_ITEMS)
        .find(|index| !baseline.contains(index))
        .unwrap();
    values[structural_index]["unexpected"] = Value::Bool(true);

    let anomalous_text = serde_json::to_string(&values).unwrap();
    let selected = select_record_indices(items(&parsed(&anomalous_text)));
    assert!(selected.contains(&error_index));
    assert!(selected.contains(&structural_index));
}

#[test]
fn record_selection_preserves_numeric_outliers_with_enough_samples() {
    let mut values = (0..100)
        .map(|index| serde_json::json!({"id": index, "latency": 10.0}))
        .collect::<Vec<_>>();
    let baseline_text = serde_json::to_string(&values).unwrap();
    let baseline = select_record_indices(items(&parsed(&baseline_text)));
    let outlier = (RECORD_HEAD_ITEMS..values.len() - RECORD_TAIL_ITEMS)
        .find(|index| !baseline.contains(index))
        .unwrap();
    values[outlier]["latency"] = serde_json::json!(10_000.0);

    let outlier_text = serde_json::to_string(&values).unwrap();
    assert!(select_record_indices(items(&parsed(&outlier_text))).contains(&outlier));
}

#[test]
fn critical_records_can_exceed_the_normal_budget() {
    let mut values = records(80);
    for record in values.iter_mut().take(60).skip(20) {
        record["status"] = Value::String("failed".to_owned());
    }

    let text = serde_json::to_string(&values).unwrap();
    let selected = select_record_indices(items(&parsed(&text)));
    assert!(selected.len() > RECORD_MAX_ITEMS);
    assert!((20..60).all(|index| selected.contains(&index)));
}

#[test]
fn ordinary_duplicates_are_removed_before_stable_sampling() {
    let text = format!("[{}]", vec![r#"{"status":"ok"}"#; 100].join(","));

    assert_eq!(
        select_record_indices(items(&parsed(&text))),
        BTreeSet::from([0, 1, 2, 3, 96, 97, 98, 99])
    );
}

#[test]
fn nested_record_array_is_reduced_in_place() {
    let store = InMemoryStore::new();
    let input = serde_json::to_string(&serde_json::json!({"batch": records(40)})).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&store)))
        .unwrap();
    let output = output_value(&outcome);

    assert_eq!(
        output["batch"].as_array().unwrap().len(),
        RECORD_MAX_ITEMS + 1
    );
    assert_eq!(outcome.metrics.record_reductions, 1);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert_eq!(
        tokenless_ccr::recovery_hashes(&outcome.output, &RecoveryMethod::Shell),
        [outcome.stash_writes[0].key.as_str()]
    );
}

#[test]
fn lossless_toon_skips_record_reduction() {
    let store = InMemoryStore::new();
    let input = serde_json::to_string(&records(100)).unwrap();
    let toon_context = JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        allow_toon: true,
        min_toon_chars: 0,
        ..context(Some(&store))
    };
    let outcome = JsonCompressor::default()
        .compress(&input, &toon_context)
        .unwrap();

    assert_eq!(outcome.operations, [JsonOperation::Toon]);
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn reduced_toon_can_win_after_lossless_toon_misses_the_gate() {
    let store = InMemoryStore::new();
    let values = (0..40)
        .map(|index| serde_json::json!({"v": format!("{index}{}", "x".repeat(100))}))
        .collect::<Vec<_>>();
    let metadata = (0..5)
        .map(|index| {
            serde_json::json!({
                "long_field_name_0": index,
                "long_field_name_1": index + 1,
                "long_field_name_2": index + 2
            })
        })
        .collect::<Vec<_>>();
    let input = serde_json::to_string(&serde_json::json!({
        "items": values,
        "metadata": metadata
    }))
    .unwrap();
    let toon_context = JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        allow_toon: true,
        min_toon_chars: 0,
        ..context(Some(&store))
    };
    let outcome = JsonCompressor::default()
        .compress(&input, &toon_context)
        .unwrap();

    assert_eq!(
        outcome.operations,
        [JsonOperation::RecordReduction, JsonOperation::Toon]
    );
    assert_eq!(outcome.metrics.record_reductions, 1);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert_eq!(
        tokenless_ccr::recovery_hashes(&outcome.output, &RecoveryMethod::Shell),
        [outcome.stash_writes[0].key.as_str()]
    );
}

#[test]
fn operations_follow_reduction_truncation_order() {
    let store = InMemoryStore::new();
    let mut values = records(40);
    for value in &mut values {
        value["empty"] = Value::Null;
    }
    values[10]["status"] = Value::String("failed".to_owned());
    values[10]["message"] = Value::String("x".repeat(5_000));
    let input = serde_json::to_string(&values).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&store)))
        .unwrap();

    assert_eq!(
        outcome.operations,
        [JsonOperation::RecordReduction, JsonOperation::Truncation,]
    );
}

#[test]
fn unrecoverable_bounded_candidate_can_be_disallowed() {
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_strings_at: 10,
        ..JsonCompressionConfig::default()
    });
    let input = format!("{{ \"tail\": \"{}\", \"unused\": null }}", "x".repeat(200));
    let restricted = JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        allow_unrecoverable: false,
        ..context(None)
    };
    let outcome = compressor.compress(&input, &restricted).unwrap();

    assert_eq!(
        output_value(&outcome),
        serde_json::json!({"tail": "x".repeat(200), "unused": null})
    );
    assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
}

#[test]
fn inexact_json_numbers_compress_without_losing_their_literals() {
    for literal in [
        "18446744073709551617",
        "-9223372036854775809",
        "1.234567890123456789",
        "1e-400",
    ] {
        let input = format!(
            "{{\n{}\"value\": {literal},\n{}\"label\": \"synthetic\"\n}}",
            " ".repeat(600),
            " ".repeat(600)
        );
        for source in [
            input.clone(),
            serde_json::to_string(&input).unwrap(),
            format!("\u{feff}{input}"),
            format!("[{input}]"),
        ] {
            let store = InMemoryStore::new();
            let outcome = JsonCompressor::default()
                .compress(&source, &context(Some(&store)))
                .unwrap();
            assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
            assert_eq!(outcome.recoverability, Recoverability::Lossless);
            assert!(outcome.stash_writes.is_empty());
            assert_eq!(outcome.metrics, JsonMetrics::default());
            // The literal survives verbatim, unquoted and unrounded, in every
            // routing variant: direct, string envelope, byte-order mark, and
            // nested inside an array.
            assert!(
                retains_number(&outcome.output, literal),
                "{literal} missing from {}",
                outcome.output
            );
            assert!(
                serde_json::from_str::<Value>(&outcome.output).is_ok(),
                "output is not JSON: {}",
                outcome.output
            );
            assert!(outcome.output.chars().count() < source.chars().count());
        }
    }
}

#[test]
fn exact_numbers_are_retained_across_integer_and_exponent_boundaries() {
    for literal in [
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
        "18446744073709551617",
        "1.234567890123456789",
        "0.1000000000000000055511151231257827",
        "12345678901234567890.0987654321",
        "5e-324",
        "1e-400",
        "1.7976931348623157e308",
    ] {
        let input = format!(
            "{{\n{}\"value\": {literal},\n{}\"label\": \"synthetic\"\n}}",
            " ".repeat(600),
            " ".repeat(600)
        );
        let outcome = JsonCompressor::default()
            .compress(&input, &context(None))
            .unwrap();
        assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
        assert!(
            retains_number(&outcome.output, literal),
            "{literal} missing from {}",
            outcome.output
        );
    }
}

#[test]
fn numbers_serde_json_cannot_read_stay_a_passthrough() {
    // Retention is only useful while the rest of the toolchain can read the
    // literal back. A number outside the finite `f64` range is such a case, so
    // the document keeps the #7069 disposition instead of being rewritten.
    for literal in ["1e400", "-1e400", "1e309", "1e999999999999999999"] {
        let input = format!(
            "{{\n{}\"value\": {literal},\n{}\"label\": \"synthetic\"\n}}",
            " ".repeat(600),
            " ".repeat(600)
        );
        let store = InMemoryStore::new();
        let outcome = JsonCompressor::default().compress(&input, &context(Some(&store)));
        assert!(outcome.is_err(), "{literal} must not be rewritten");
        assert!(store.retrieve("x").unwrap().is_none());
    }
}

#[test]
fn equivalent_decimal_spellings_and_numeric_strings_still_compress() {
    for literal in [
        "18446744073709551615",
        "1e3",
        "1000.00",
        "1.2500",
        "-0",
        "42",
    ] {
        let label = serde_json::to_string(r#"数字 18446744073709551617 \"1e-400\" \\"#).unwrap();
        let input = format!(
            "{{\n{}\"value\": {literal},\n{}\"label\": {label}\n}}",
            " ".repeat(600),
            " ".repeat(600)
        );
        let outcome = JsonCompressor::default()
            .compress(&input, &context(None))
            .unwrap();
        assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
        assert_eq!(outcome.recoverability, Recoverability::Lossless);
        assert_eq!(
            output_value(&outcome),
            serde_json::from_str::<Value>(&input).unwrap()
        );
        // Compaction does not normalize an equivalent spelling: the source
        // token is what reaches the model.
        assert!(
            retains_number(&outcome.output, literal),
            "{literal} missing from {}",
            outcome.output
        );
    }
}

#[test]
fn numeric_strings_and_reserved_key_objects_keep_their_shape() {
    let padding = "x".repeat(4096);
    let input = format!(
        "{{\n \"big\": \"18446744073709551617\",\n \"raw\": {{\"$serde_json::private::RawValue\":\"123\"}},\n \"number\": {{\"$serde_json::private::Number\":\"456\"}},\n \"padding\": \"{padding}\"\n}}"
    );
    let outcome = JsonCompressor::default()
        .compress(&input, &context(None))
        .unwrap();
    let output = output_value(&outcome);
    assert_eq!(
        output["big"],
        Value::String("18446744073709551617".to_owned())
    );
    assert_eq!(
        output["raw"],
        serde_json::json!({"$serde_json::private::RawValue": "123"})
    );
    assert_eq!(
        output["number"],
        serde_json::json!({"$serde_json::private::Number": "456"})
    );
    assert_eq!(output["padding"], Value::String(padding));
}

#[test]
fn stashed_omissions_carry_exact_numbers() {
    let literal = "1.234567890123456789";

    // Omitted array items.
    let store = InMemoryStore::new();
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_arrays_at: 3,
        array_tail_preserve: 0,
        ..JsonCompressionConfig::default()
    });
    let input = format!(
        "[{}]",
        (0..10)
            .map(|index| format!("{{\"id\":{index},\"value\":{literal}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let outcome = compressor.compress(&input, &context(Some(&store))).unwrap();
    let payload = store
        .retrieve(extract_hash(&outcome.output).unwrap())
        .unwrap()
        .expect("omitted items are stashed");
    assert_eq!(
        serde_json::from_str::<Value>(&payload)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        7
    );
    assert_eq!(payload.matches(literal).count(), 7);

    // Complete record collection behind a reduction marker.
    let store = InMemoryStore::new();
    let input = format!(
        "[{}]",
        (0..40)
            .map(|index| format!("{{\"id\":{index},\"value\":{literal}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&store)))
        .unwrap();
    assert!(outcome.metrics.records_omitted > 0);
    let payload = store
        .retrieve(extract_hash(&outcome.output).unwrap())
        .unwrap()
        .expect("record collection is stashed");
    assert_eq!(
        serde_json::from_str::<Value>(&payload)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        40
    );
    assert_eq!(payload.matches(literal).count(), 40);

    // Depth-truncated subtree.
    let store = InMemoryStore::new();
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        max_depth: 0,
        ..JsonCompressionConfig::default()
    });
    let input = format!(
        "{{\"nested\":{{\"padding\":\"{}\",\"value\":{literal}}}}}",
        "x".repeat(600)
    );
    let outcome = compressor.compress(&input, &context(Some(&store))).unwrap();
    let payload = store
        .retrieve(extract_hash(&outcome.output).unwrap())
        .unwrap()
        .expect("subtree is stashed");
    assert_eq!(
        serde_json::from_str::<Value>(&payload)
            .unwrap()
            .get("padding")
            .and_then(Value::as_str)
            .map(str::len),
        Some(600)
    );
    assert!(payload.ends_with(&format!("\"value\":{literal}}}")));
}

#[test]
fn record_deduplication_sees_through_numeric_spelling() {
    let uniform = format!(
        "[{}]",
        vec![r#"{"status":"ok","value":1.2500}"#; 100].join(",")
    );
    let mixed = format!(
        "[{}]",
        (0..100)
            .map(|index| if index == 50 {
                r#"{"status":"ok","value":1.25}"#.to_owned()
            } else {
                r#"{"status":"ok","value":1.2500}"#.to_owned()
            })
            .collect::<Vec<_>>()
            .join(",")
    );
    let expected = BTreeSet::from([0, 1, 2, 3, 96, 97, 98, 99]);
    assert_eq!(select_record_indices(items(&parsed(&uniform))), expected);
    assert_eq!(select_record_indices(items(&parsed(&mixed))), expected);

    // A different value is not a duplicate, so it competes for sample slots.
    let distinct = format!(
        "[{}]",
        (0..100)
            .map(|index| if index == 50 {
                r#"{"status":"ok","value":1.26}"#.to_owned()
            } else {
                r#"{"status":"ok","value":1.2500}"#.to_owned()
            })
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(select_record_indices(items(&parsed(&distinct))).len() > expected.len());
}

fn toon_context<'a>(stash: Option<&'a dyn StashStore>) -> JsonCompressionContext<'a> {
    JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        stash,
        allow_toon: true,
        min_toon_chars: 0,
        allow_unrecoverable: true,
    }
}

fn record_rows(literal: &str, count: usize) -> String {
    format!(
        r#"{{"rows":[{}]}}"#,
        (0..count)
            .map(|index| format!(r#"{{"id":{index},"value":{literal}}}"#))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[test]
fn toon_is_rejected_when_it_cannot_retain_exact_numbers() {
    for literal in [
        "1.234567890123456789",
        "18446744073709551617",
        "-9223372036854775809",
        "1e-400",
        "1e3",
        "-0",
    ] {
        let input = record_rows(literal, 40);
        let outcome = JsonCompressor::default()
            .compress(&input, &toon_context(None))
            .unwrap();
        assert!(
            !outcome.operations.contains(&JsonOperation::Toon),
            "{literal} must not be represented by TOON"
        );
        assert!(
            retains_number(&outcome.output, literal),
            "{literal} missing from {}",
            outcome.output
        );
        // The compacted JSON candidate is kept, so compression is not lost.
        assert!(outcome.output.chars().count() < input.chars().count());
    }
}

#[test]
fn toon_still_wins_for_numbers_it_retains_exactly() {
    for literal in ["1.25", "42", "18446744073709551615", "-9223372036854775808"] {
        let input = record_rows(literal, 40);
        let outcome = JsonCompressor::default()
            .compress(&input, &toon_context(None))
            .unwrap();
        assert!(
            outcome.operations.contains(&JsonOperation::Toon),
            "{literal} should still convert: {}",
            outcome.output
        );
        let decoded: Value = toon_format::decode_default(&outcome.output).unwrap();
        let expected = parsed(&input);
        assert!(exact::matches_exact(&decoded, &expected));
    }
}

#[test]
fn toon_may_normalize_an_equivalent_spelling() {
    // Documented policy: TOON may shorten `1.2500` to `1.25` because the
    // decimal value and the float type are unchanged.
    let input = record_rows("1.2500", 40);
    let outcome = JsonCompressor::default()
        .compress(&input, &toon_context(None))
        .unwrap();
    assert!(outcome.operations.contains(&JsonOperation::Toon));
    let decoded: Value = toon_format::decode_default(&outcome.output).unwrap();
    assert!(exact::matches_exact(&decoded, &parsed(&input)));
    assert_eq!(decoded, serde_json::from_str::<Value>(&input).unwrap());
}

#[test]
fn toon_never_normalizes_negative_zero() {
    // Documented policy: `-0` keeps its sign. TOON reads it back as `0`, so the
    // exact JSON candidate is kept instead.
    let input = record_rows("-0", 40);
    let outcome = JsonCompressor::default()
        .compress(&input, &toon_context(None))
        .unwrap();
    assert!(!outcome.operations.contains(&JsonOperation::Toon));
    assert!(retains_number(&outcome.output, "-0"));
}

#[test]
fn reserved_number_key_remains_an_object() {
    let input = "{\"value\":{\"$serde_json::private::Number\":\"123\"},\"tail\":null}";
    let outcome = JsonCompressor::default()
        .compress(input, &context(None))
        .unwrap();
    assert_eq!(
        output_value(&outcome)["value"],
        serde_json::json!({"$serde_json::private::Number":"123"})
    );
}

#[test]
fn toon_candidate_preserves_large_integers_and_types() {
    for literal in [
        "9007199254740993",
        "18446744073709551615",
        "-9223372036854775808",
    ] {
        let text = format!("{{\"nested\":[{{\"value\":{literal}}}],\"string\":\"{literal}\"}}");
        let document = parsed(&text);
        let compact = exact::write_to_string(&document);
        if let Some(candidate) = toon_candidate(&document, &compact, 0) {
            let decoded: Value = toon_format::decode_default(&candidate).unwrap();
            assert!(exact::matches_exact(&decoded, &document));
        }
    }
    let text = r#"{"rows":[{"id":42,"value":1.25},{"id":43,"value":2.5}]}"#;
    let document = parsed(text);
    let compact = exact::write_to_string(&document);
    let candidate =
        toon_candidate(&document, &compact, 0).expect("ordinary numbers remain convertible");
    let decoded: Value = toon_format::decode_default(&candidate).unwrap();
    assert!(exact::matches_exact(&decoded, &document));
}

#[test]
fn record_reduction_keeps_retained_numbers_exact() {
    let literal = "18446744073709551615";
    let input = format!(
        "[{}]",
        (0..40)
            .map(|index| format!("{{\"id\":{index},\"detail\":{{\"value\":{literal}}}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let store = InMemoryStore::new();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&store)))
        .unwrap();
    assert!(outcome.metrics.records_omitted > 0);
    let output = output_value(&outcome);
    let retained = output
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row.get("detail").is_some())
        .collect::<Vec<_>>();
    assert!(!retained.is_empty());
    for row in retained {
        assert!(row["detail"]["value"].is_number());
        assert_eq!(row["detail"]["value"].to_string(), literal);
    }
}
