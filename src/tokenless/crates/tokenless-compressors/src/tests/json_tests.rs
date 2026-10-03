use std::sync::Arc;

use serde_json::Value;
use tokenless_ccr::{InMemoryStore, StashError, StashStore, StashWrite, extract_hash};

fn context<'a>(stash: Option<&'a dyn StashStore>) -> JsonCompressionContext<'a> {
    JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        stash,
        allow_toon: false,
        preserve_top_level_shape: false,
        min_toon_chars: 500,
        allow_unrecoverable: true,
    }
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
fn cleanup_reports_the_operation_without_side_channels() {
    let outcome = JsonCompressor::default()
        .compress(
            r#"{"data":"kept","debug":"drop","empty":null}"#,
            &context(None),
        )
        .unwrap();
    assert_eq!(outcome.output, r#"{"data":"kept"}"#);
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
fn drop_nulls_is_independent_from_empty_value_cleanup() {
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        drop_nulls: false,
        drop_empty_fields: true,
        ..JsonCompressionConfig::default()
    });
    let outcome = compressor
        .compress(
            r#"{"object_null":null,"empty":"","array":[null,""]}"#,
            &context(None),
        )
        .unwrap();

    assert_eq!(
        output_value(&outcome),
        serde_json::json!({"object_null": null, "array": [null]})
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
fn structured_slots_restore_empty_top_level_fields() {
    let context = JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        preserve_top_level_shape: true,
        ..context(None)
    };
    let outcome = JsonCompressor::default()
        .compress(r#"{"stdout":"value","stderr":"","debug":"drop"}"#, &context)
        .unwrap();
    assert_eq!(
        output_value(&outcome),
        serde_json::json!({"stdout": "value", "stderr": ""})
    );
}

#[test]
fn json_string_envelope_is_normalized_once() {
    let input = serde_json::to_string(r#"{"data":"kept","debug":"drop"}"#).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(None))
        .unwrap();
    assert_eq!(outcome.output, r#"{"data":"kept"}"#);
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
    assert!(!is_record_array(&records(32)));
    assert!(is_record_array(&records(33)));
    let mut mixed = records(33);
    mixed[16] = Value::Null;
    assert!(!is_record_array(&mixed));
}

#[test]
fn error_signals_require_nonempty_fields_or_matching_status_text() {
    assert!(!has_error_signal(
        serde_json::json!({"error": null, "status": "ok"})
            .as_object()
            .unwrap()
    ));
    assert!(has_error_signal(
        serde_json::json!({"errors": ["boom"]}).as_object().unwrap()
    ));
    assert!(has_error_signal(
        serde_json::json!({"severity": "Critical warning"})
            .as_object()
            .unwrap()
    ));
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
fn record_reduction_counts_selected_records_removed_by_cleanup() {
    let store = InMemoryStore::new();
    let mut values = records(40);
    // This head-window record collapses to an empty object during cleanup, so
    // it is selected for retention but never emitted.
    values[0] = serde_json::json!({ "note": null });
    let input = serde_json::to_string(&values).unwrap();
    let outcome = JsonCompressor::default()
        .compress(&input, &context(Some(&store)))
        .unwrap();

    let array = output_value(&outcome);
    let array = array.as_array().unwrap();
    let kept = array.iter().filter(|value| value.is_object()).count();
    assert_eq!(kept, RECORD_MAX_ITEMS - 1);
    assert_eq!(
        outcome.operations,
        [JsonOperation::Cleanup, JsonOperation::RecordReduction]
    );
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
    let baseline = select_record_indices(&values);
    let error_index = (RECORD_HEAD_ITEMS..values.len() - RECORD_TAIL_ITEMS)
        .find(|index| !baseline.contains(index))
        .unwrap();
    values[error_index]["status"] = Value::String("WARNING".to_owned());
    let structural_index = (error_index + 1..values.len() - RECORD_TAIL_ITEMS)
        .find(|index| !baseline.contains(index))
        .unwrap();
    values[structural_index]["unexpected"] = Value::Bool(true);

    let selected = select_record_indices(&values);
    assert!(selected.contains(&error_index));
    assert!(selected.contains(&structural_index));
}

#[test]
fn record_selection_preserves_numeric_outliers_with_enough_samples() {
    let mut values = (0..100)
        .map(|index| serde_json::json!({"id": index, "latency": 10.0}))
        .collect::<Vec<_>>();
    let baseline = select_record_indices(&values);
    let outlier = (RECORD_HEAD_ITEMS..values.len() - RECORD_TAIL_ITEMS)
        .find(|index| !baseline.contains(index))
        .unwrap();
    values[outlier]["latency"] = serde_json::json!(10_000.0);

    assert!(select_record_indices(&values).contains(&outlier));
}

#[test]
fn critical_records_can_exceed_the_normal_budget() {
    let mut values = records(80);
    for record in values.iter_mut().take(60).skip(20) {
        record["status"] = Value::String("failed".to_owned());
    }

    let selected = select_record_indices(&values);
    assert!(selected.len() > RECORD_MAX_ITEMS);
    assert!((20..60).all(|index| selected.contains(&index)));
}

#[test]
fn ordinary_duplicates_are_removed_before_stable_sampling() {
    let values = (0..100)
        .map(|_| serde_json::json!({"status": "ok"}))
        .collect::<Vec<_>>();

    assert_eq!(
        select_record_indices(&values),
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
fn operations_follow_cleanup_reduction_truncation_toon_order() {
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
        [
            JsonOperation::Cleanup,
            JsonOperation::RecordReduction,
            JsonOperation::Truncation,
        ]
    );
}

#[test]
fn unrecoverable_bounded_candidate_can_be_disallowed() {
    let compressor = JsonCompressor::new(JsonCompressionConfig {
        truncate_strings_at: 10,
        ..JsonCompressionConfig::default()
    });
    let input = serde_json::to_string(&serde_json::json!({
        "tail": "x".repeat(200),
        "unused": null
    }))
    .unwrap();
    let restricted = JsonCompressionContext {
        recovery: &tokenless_protocol::RecoveryMethod::Shell,
        allow_unrecoverable: false,
        ..context(None)
    };
    let outcome = compressor.compress(&input, &restricted).unwrap();

    assert_eq!(
        output_value(&outcome),
        serde_json::json!({"tail": "x".repeat(200)})
    );
    assert_eq!(outcome.operations, [JsonOperation::Cleanup]);
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
}
