# v1 to v2 Database Test Migration Ledger

Every database-related test in v1 `agent-sec-cli` is accounted for here. The
ledger answers one question per v1 case: **which v2 test now covers this
behaviour**, and if none does, **why that is acceptable**.

This is not the same check as the migration-time differential harness. That local
tool ran the two implementations side by side and diffed their output — it proved
*agreement*. This ledger proves *coverage*: that v2's own branch coverage did not
shrink when the tests were rewritten.

## How to read this

| Mode | Meaning |
|---|---|
| `1:1` | One v1 case, one v2 test. |
| `merged` | Several v1 cases collapse into one v2 test, usually because v1 repeated the same assertion with a different category or verdict. |
| `split` | One v1 case became several v2 tests. |
| `moved` | The behaviour moved to a different layer than v1 had it (typically v1 tested through a facade, v2 tests the kernel). |
| `by construction` | v2's types make the v1 failure unrepresentable; the listed test pins the construction. |
| `not migrated` | Out of scope, with the reason stated. |

Test names are given without the `crate::` prefix; the owning crate heads each
section. Regenerate the inventories with:

```bash
# v1
grep -cE '^\s*(async )?def test_' tests/unit-test/security_events/test_writer.py
# v2
cargo +1.93.1 test -p asc-event-log -- --list | grep -c ': test$'
```

## Totals

| | v1 | v2 |
|---|---|---|
| Scope | 16 test files | 7 crates |
| Cases | 322 | **364** |

Per-crate v2 counts, as of this ledger:

| Crate | Tests |
|---|---|
| `asc-security-events` | 38 |
| `asc-observability` | 34 |
| `asc-event-log` | 32 |
| `asc-sqlite-kernel` | 69 |
| `asc-persistence-sqlite` | 109 |
| `asc-security-summary` | 63 |
| `asc-event-sink` | 19 |

The count grew rather than shrank for two reasons: v2 splits what v1 tested
through a facade into kernel-level and binding-level cases, and the gaps this
ledger surfaced were closed by writing the missing tests (see
[Gaps closed while writing this ledger](#gaps-closed-while-writing-this-ledger)).

## Deprecations

The plan permits exactly two, both `SQLAlchemy` implementation details:

| v1 case | Reason |
|---|---|
| `test_orm_store::test_split_model_modules_import_in_either_order` | Asserts that `register_orm_models` produces the same global registry whichever module is imported first. v2 has no global registry: every store receives its `&[TableSpec]` explicitly, so import order cannot influence anything. |
| `test_orm_store::test_ensure_schema_rejects_empty_default_models` | Asserts the rejection of an *empty default* model set. v2 has no defaults to fall back to; the equivalent guard is "reject an empty table slice", which `store::tests::rejects_an_empty_table_slice` covers. Demoted rather than dropped. |

One further v1 case is **not migrated** but was declared out of scope by the plan
rather than deprecated here:

| v1 case | Reason |
|---|---|
| `test_timestamp_display::test_cli_table_timestamp_uses_local_timezone` | Exercises `cli._format_timestamp`, a caller of the summary layer. `cli.py` is not part of this migration. |

## Serial cases

`cargo test` runs tests multi-threaded in one process, so anything touching
process-global state must be serialized. Only `asc-event-sink` has such state —
the four `OnceLock` sink slots — and all fifteen of its stateful tests take
`test_support::serial()`, a process-wide `Mutex`. No `--test-threads=1` is
needed anywhere.

| File | Serialized tests |
|---|---|
| `asc-event-sink/src/lib.rs` | `merely_linking_this_crate_initializes_nothing` |
| `asc-event-sink/src/singletons.rs` | `an_untouched_slot_reports_nothing_initialized`, `repeated_access_returns_the_same_instance`, `a_reset_slot_forgets_its_value_without_closing_it` |
| `asc-event-sink/src/security_events.rs` | `both_paths_receive_the_event`, `a_broken_jsonl_path_does_not_stop_the_sqlite_insert`, `a_broken_database_does_not_stop_the_jsonl_append`, `both_paths_broken_is_still_silent` |
| `asc-event-sink/src/observability.rs` | `both_paths_receive_the_record`, `a_broken_jsonl_path_surfaces_and_skips_the_sqlite_insert`, `a_broken_database_surfaces_after_the_jsonl_append` |
| `asc-event-sink/src/shutdown.rs` | `shutting_down_untouched_sinks_creates_nothing`, `shutdown_runs_the_maintenance_pass_once_per_window`, `no_shutdown_means_no_maintenance`, `both_streams_are_closed` |

Everything else takes an explicit `TempDir` path through a constructor argument
and reads no environment variable. `asc-security-events::config` — the one place
that resolves paths from the environment in production — is tested through an
injected `Env` struct, so even the three-tier fallback needs no serialization.

## security_events/test_schema.py (17) → `asc-security-events`

| v1 case | Mode | v2 test |
|---|---|---|
| `required_fields_set` | merged | `event::tests::defaults_match_v1` |
| `event_id_is_uuid` | merged | `event::tests::defaults_match_v1` |
| `timestamp_is_iso8601` | merged | `event::tests::defaults_match_v1` |
| `timestamp_is_normalized_to_utc` | split | `event::tests::deserialization_normalizes_offset_timestamp_to_utc`, `timestamp::tests::offset_is_converted_to_utc` |
| `empty_timestamp_uses_generated_utc_timestamp` | 1:1 | `event::tests::deserialization_replaces_empty_timestamp` |
| `invalid_timestamp_is_rejected` | split | `event::tests::deserialization_rejects_invalid_timestamp`, `timestamp::tests::invalid_input_is_reported_with_field_and_value` |
| `pid_matches_current` | merged | `event::tests::defaults_match_v1` |
| `uid_matches_current` | merged | `event::tests::defaults_match_v1` |
| `result_default_succeeded` | merged | `event::tests::defaults_match_v1` |
| `result_can_be_failed` | 1:1 | `event::tests::result_serializes_as_lowercase_literal` |
| `trace_id_default_empty` | merged | `event::tests::defaults_match_v1` |
| `session_id_default_none` | merged | `event::tests::optional_fields_serialize_as_null_like_v1` |
| `agent_trace_fields_default_none` | merged | `event::tests::optional_fields_serialize_as_null_like_v1` |
| `to_dict_has_all_keys` | merged | `event::tests::serialized_key_order_matches_v1_to_dict` |
| `to_dict_omits_agent_name_extra_field` | by construction | `event::tests::serialized_key_order_matches_v1_to_dict` — v2's struct has no extra-field bag, so an unknown field cannot reach the serializer. |
| `to_dict_includes_top_level_tracing_fields` | merged | `event::tests::serialized_key_order_matches_v1_to_dict` |
| `to_dict_roundtrip_json` | split | `event::tests::round_trip_preserves_all_fields`, `event::tests::non_ascii_payload_is_not_escaped` |

Beyond v1: `extract_verdict_*` (3), `set_timestamp_normalizes_and_rejects`,
`schema_version::tests` (3), `summary::tests::empty_summary_is_all_zero`, and the
nine remaining `timestamp::tests` — the epoch conversion helpers v1 kept inline
in its ORM layer.

## security_events/test_config.py (8) → `asc-security-events::config`

| v1 case | Mode | v2 test |
|---|---|---|
| `primary_path_when_writable` | 1:1 | `config::tests::primary_tier_wins_when_writable` |
| `fallback_when_primary_not_writable` | 1:1 | `config::tests::home_tier_is_used_when_primary_cannot_be_created` |
| `fallback_when_makedirs_fails` | split | `config::tests::tmp_tier_is_used_when_primary_and_home_fail`, `config::tests::last_resort_path_is_returned_without_being_created` |
| `db_path_uses_primary_dir` | merged | `config::tests::stream_paths_are_built_under_the_data_dir` |
| `db_path_uses_fallback_dir` | merged | `config::tests::stream_paths_are_built_under_the_data_dir` |
| `env_override_resolves_stream_specific_paths` | split | `config::tests::override_wins_over_every_tier_and_is_created_0700`, `config::tests::unusable_override_is_a_hard_error` |
| `security_event_paths_remain_default_stream` | merged | `config::tests::stream_paths_are_built_under_the_data_dir` |
| `stream_names_reject_path_traversal` | split | `config::tests::stream_path_helpers_reject_invalid_names`, `config::tests::stream_names_follow_v1_regex`, `config::tests::invalid_stream_name_message_matches_v1` |

Beyond v1: `tmp_tier_rejects_a_directory_owned_by_another_uid` — v1 resolves the
`/tmp` tier without checking ownership.

## security_events/test_log_event.py (8) → `asc-event-sink`

| v1 case | Mode | v2 test |
|---|---|---|
| `security_events_package_import_does_not_load_sqlalchemy` | split | `tests::merely_linking_this_crate_initializes_nothing`, `asc-sqlite-kernel tests::cargo_manifest_has_no_domain_dependency` |
| `singleton_returns_same_instance` (JSONL) | merged | `singletons::tests::repeated_access_returns_the_same_instance` |
| `log_event_delegates_to_writer` | merged | `security_events::tests::both_paths_receive_the_event` |
| `log_event_swallows_exceptions` | 1:1 | `security_events::tests::both_paths_broken_is_still_silent` |
| `singleton_returns_same_instance` (`SQLite`) | merged | `singletons::tests::repeated_access_returns_the_same_instance` |
| `log_event_writes_to_both` | merged | `security_events::tests::both_paths_receive_the_event` |
| `jsonl_failure_does_not_block_sqlite` | 1:1 | `security_events::tests::a_broken_jsonl_path_does_not_stop_the_sqlite_insert` |
| `sqlite_failure_does_not_block_jsonl` | 1:1 | `security_events::tests::a_broken_database_does_not_stop_the_jsonl_append` |

Beyond v1: the four `shutdown::tests` (v1 relies on `atexit` and never tests the
close path), `singletons::tests::an_untouched_slot_reports_nothing_initialized`,
`a_reset_slot_forgets_its_value_without_closing_it`, `tests::the_shared_sinks_are_send`,
and the three `api_parity` meta-tests.

## security_events/test_sqlite_maintenance.py (4) → `asc-sqlite-kernel::maintenance`

| v1 case | Mode | v2 test |
|---|---|---|
| `sqlite_maintenance_runs_once_per_interval` | split | `maintenance::tests::first_run_is_due_and_writes_the_marker`, `a_second_run_inside_the_interval_is_skipped`, `the_gate_opens_again_exactly_at_the_interval` |
| `sqlite_maintenance_skips_when_another_process_holds_lock` | 1:1 | `maintenance::tests::a_held_lock_causes_a_skip` |
| `sqlite_maintenance_retries_after_callback_failure` | 1:1 | `maintenance::tests::a_failing_maintenance_does_not_advance_the_gate` |
| `sqlite_maintenance_treats_invalid_marker_as_due` | split | `maintenance::tests::a_corrupt_marker_is_treated_as_absent`, `a_marker_in_the_future_is_treated_as_stale` |

Beyond v1: `a_non_positive_interval_always_runs`, `marker_and_lock_names_match_v1`,
`the_default_interval_is_one_day`.

## security_events/test_timestamp_display.py (3) → `asc-security-summary::time_fmt`

| v1 case | Mode | v2 test |
|---|---|---|
| `cli_table_timestamp_uses_local_timezone` | not migrated | Tests `cli._format_timestamp`; `cli.py` is a caller, out of scope. |
| `summary_timestamp_uses_local_timezone` | 1:1 | `time_fmt::tests::a_parsable_timestamp_renders_in_local_time` |
| `naive_timestamp_is_treated_as_utc_for_display` | 1:1 | `time_fmt::tests::an_offset_less_timestamp_is_read_as_utc` |

Beyond v1: `every_age_bucket_is_reachable`, `a_future_timestamp_reads_as_just_now`,
`an_unparsable_timestamp_has_an_unknown_age`, `an_unparsable_timestamp_is_shown_verbatim`.

## security_events/test_repositories.py (1) → `asc-persistence-sqlite`

| v1 case | Mode | v2 test |
|---|---|---|
| `query_correlation_candidates_logs_warning_on_sqlalchemy_error` | moved | `asc-sqlite-kernel query::tests::a_failing_query_degrades_to_the_default`, `security_events_store::a_missing_database_degrades_to_empty_reads` |

v1 injects the failure with a `MagicMock` that raises `OperationalError`. v2 has
no mocking framework and does not want one here: the kernel test degrades a real
query against a real database, which also proves the fallback value is returned
rather than merely that a warning was logged.

## security_events/test_orm_store.py (26) → `asc-sqlite-kernel`

| v1 case | Mode | v2 test |
|---|---|---|
| `schema_version_tracks_revision_history` | moved | `asc-security-events schema_version::tests::schema_version_tracks_revision_history` |
| `sqlite_corruption_classification_uses_result_code` | 1:1 | `error::tests::corruption_codes_match_v1` |
| `sqlite_busy_classification_requires_result_code` | split | `error::tests::busy_codes_match_v1`, `error::tests::non_sqlite_errors_have_no_primary_code` |
| `write_engine_preserves_sqlite_pragmas` | split | `connection::tests::writable_connection_applies_the_v1_pragma_set`, `schema::tests::auto_vacuum_stays_none_exactly_as_in_v1` |
| `readonly_engine_uses_sqlite_readonly_uri` | split | `connection::tests::read_only_connection_sets_query_only_and_rejects_writes`, `uri_special_characters_are_escaped`, `read_only_open_works_for_a_path_with_a_question_mark` |
| `normalize_sqlite_path_expands_user` | split | `path::tests::expands_a_leading_tilde`, and the four other `path::tests` |
| `split_model_modules_import_in_either_order` | **deprecated** | See [Deprecations](#deprecations). |
| `ensure_schema_creates_registered_model_tables_and_indexes` | split | `store::tests::writable_store_creates_schema_and_tightens_modes`, `security_events_store::the_converged_schema_carries_every_declared_column_and_index` |
| `ensure_schema_rejects_empty_default_models` | **deprecated** | Demoted to `store::tests::rejects_an_empty_table_slice`. |
| `sqlite_store_rejects_explicit_empty_models` | 1:1 | `store::tests::rejects_an_empty_table_slice` |
| `ensure_schema_does_not_downgrade_newer_schema` | merged | `v1_fixtures::a_newer_schema_version_is_left_untouched` |
| `ensure_schema_if_needed_does_not_downgrade_newer_schema` | merged | `v1_fixtures::a_newer_schema_version_is_left_untouched` |
| `ensure_schema_if_needed_skips_full_schema_when_current` | merged | `v1_fixtures::an_observability_fixture_keeps_its_only_revision` |
| `ensure_schema_if_needed_trusts_current_version_fast_path` | merged | `v1_fixtures::an_observability_fixture_keeps_its_only_revision` |
| `ensure_schema_if_needed_force_repairs_current_version_schema` | 1:1 | `store::tests::repair_request_forces_convergence_and_survives_until_used` |
| `ensure_schema_if_needed_runs_full_schema_when_version_mismatch` | 1:1 | `v1_fixtures::a_revision_one_database_converges_to_the_current_schema` |
| `sqlite_schema_error_classification_uses_message` | 1:1 | `error::tests::schema_detection_uses_code_then_markers` |
| `sqlite_store_reuses_session_factory_across_repositories` | merged | `store::tests::writable_store_caches_the_connection` |
| `security_event_prune_disposes_store_on_sqlalchemy_error` | moved | `security_events::writer::tests::a_failing_retention_pass_is_swallowed_and_still_marks_the_gate` — v2 does not dispose, and the divergence is deliberate (D-25). |
| `write_store_returns_checked_session_factory_if_cache_is_cleared` | merged | `store::tests::writable_store_caches_the_connection` |
| `request_schema_repair_is_preserved_during_concurrent_open` | 1:1 | `store::tests::repair_request_forces_convergence_and_survives_until_used` |
| `readonly_store_does_not_create_missing_db` | split | `store::tests::read_only_store_never_creates_the_database`, `connection::tests::read_only_open_fails_for_a_missing_file` |
| `write_store_chmods_only_created_parent_dirs` | 1:1 | `store::tests::writable_store_creates_schema_and_tightens_modes` |
| `readonly_store_warns_without_migrating_unready_schema` | 1:1 | `store::tests::read_only_store_does_not_migrate_an_unready_schema` |
| `sqlite_store_uses_custom_error_prefix` | split | `security_events::writer::tests::the_defaults_match_v1`, `observability::writer::tests::the_defaults_match_v1`, `security_events::policy::tests::the_three_messages_match_v1_verbatim` |
| `store_corruption_cleanup_resets_state_and_allows_reinit` | split | `store::tests::corruption_handling_deletes_all_three_files`, `a_corrupt_database_is_rebuilt_on_open`, `disabled_store_returns_none_or_errors` |

Beyond v1: `path::tests::sidecars_are_string_suffixes_not_extensions` (the
`.db-wal` naming trap), `schema::tests::identifier_rule_rejects_digits_like_v1`,
`ddl_rendering_keeps_declaration_order`, `multi_column_index_keeps_column_order`,
`python_list_formatting_matches_v1_warning`, `migration::tests::a_migrator_is_object_safe_and_shareable`,
and the three architecture guards (`cargo_manifest_has_no_domain_dependency`,
`source_files_carry_no_domain_nouns`, `long_lived_types_are_send`).

## security_events/test_sqlite_writer.py (32) → `asc-sqlite-kernel` + `asc-persistence-sqlite`

| v1 case | Mode | v2 test |
|---|---|---|
| `write_with_invalid_timestamp` | 1:1 | `security_events_store::an_unserializable_timestamp_reports_a_skipped_write` |
| `write_with_non_serializable_details` | split | `security_events_store::an_unserializable_timestamp_reports_a_skipped_write`, `write_ladder::a_malformed_record_never_retries` |
| `write_column_values_are_correct` | split | `security_events_store::a_written_event_round_trips_through_a_read_only_source`, `security_events::table::tests::column_order_matches_v1` |
| `write_creates_db_and_inserts` | 1:1 | `security_events::writer::tests::a_write_creates_the_database_lazily` |
| `write_persists_schema_normalized_utc_timestamp` | merged | `security_events_store::a_written_event_round_trips_through_a_read_only_source` |
| `db_file_permissions_are_restrictive` | 1:1 | `store::tests::writable_store_creates_schema_and_tightens_modes` |
| `wal_mode_enabled` | 1:1 | `connection::tests::writable_connection_applies_the_v1_pragma_set` |
| `fire_and_forget_never_raises` | split | `write_ladder::write_swallows_everything_the_policy_surfaces`, `security_events::policy::tests::every_outcome_is_swallowed` |
| `write_swallows_unexpected_insert_exception` | merged | `write_ladder::write_swallows_everything_the_policy_surfaces` |
| `insert_or_ignore_dedup` | split | `security_events_store::a_duplicate_event_id_is_not_a_dropped_write`, `repository::tests::insert_reports_whether_a_row_landed` |
| `thread_safety` | 1:1 | `security_events_store::ten_threads_sharing_one_sink_lose_no_rows` |
| `concurrent_writes_from_independent_writers` | 1:1 | `security_events_store::independent_sinks_on_one_database_only_lose_rows_to_busy` |
| `concurrent_cold_bootstrap_is_best_effort` | 1:1 | `security_events_store::a_cold_bootstrap_race_keeps_the_database_usable` |
| `pruning_at_close` | split | `security_events_store::retention_prunes_by_timestamp_epoch`, `write_ladder::close_runs_the_gated_maintenance_pass` |
| `corruption_detection_and_rebuild` | split | `store::tests::a_corrupt_database_is_rebuilt_on_open`, `write_ladder::corruption_retries_once_and_can_succeed` |
| `schema_migration_adds_columns` | 1:1 | `security_events::migration::tests::the_column_is_added_and_backfilled_from_both_shapes` |
| `security_events_has_tracing_columns_and_indexes` | split | `security_events::table::tests::index_names_and_column_order_match_v1`, `security_events_store::the_converged_schema_carries_every_declared_column_and_index` |
| `v1_database_migrates_on_write_and_preserves_old_rows` | split | `security_events_store::a_revision_one_database_is_lifted_by_generic_convergence`, `v1_fixtures::a_revision_one_database_converges_to_the_current_schema` |
| `v2_database_migrates_verdict_column_and_backfills` | split | `security_events_store::a_revision_two_database_is_backfilled_by_the_migrator`, `v1_fixtures::a_revision_two_database_gains_a_backfilled_verdict` |
| `v2_database_migrates_verdict_column_in_batches` | 1:1 | `security_events::migration::tests::a_batch_larger_than_the_limit_is_fully_covered` |
| `schema_repairs_missing_indexes` | 1:1 | `store::tests::repair_request_forces_convergence_and_survives_until_used` |
| `schema_error_requests_repair_for_next_write` | split | `write_ladder::schema_drift_requests_a_repair`, `security_events::policy::tests::schema_and_database_faults_keep_the_connection` |
| `close_performs_checkpoint` | merged | `write_ladder::close_runs_the_gated_maintenance_pass` |
| `close_runs_prune_and_checkpoint_through_maintenance_gate` | merged | `write_ladder::close_runs_the_gated_maintenance_pass` |
| `close_skips_repeated_maintenance_for_same_db_path` | split | `write_ladder::close_on_an_unopened_store_runs_no_maintenance`, `maintenance::tests::a_second_run_inside_the_interval_is_skipped` |
| `disabled_after_delete_failure` | 1:1 | `store::tests::disabled_store_returns_none_or_errors` |
| `write_retries_after_corruption_error` | 1:1 | `write_ladder::corruption_retries_once_and_can_succeed` |
| `write_logs_busy_insert_loss` | split | `write_ladder::a_busy_database_is_reported_as_busy`, `security_events::policy::tests::a_busy_fault_switches_the_message` |
| `write_logs_busy_session_factory_loss` | merged | `write_ladder::a_retry_that_stays_busy_carries_the_busy_flag` |
| `write_logs_busy_corruption_retry_loss` | 1:1 | `security_events::policy::tests::a_busy_corruption_retry_keeps_the_connection` |
| `write_disposes_on_corruption_retry_error` | 1:1 | `security_events::policy::tests::a_malformed_corruption_retry_disposes` |
| `write_disposes_on_sqlalchemy_error` | 1:1 | `security_events::policy::tests::an_io_fault_disposes` |

Beyond v1: the remaining `write_ladder` steps (`a_successful_write_reports_no_fault`,
`an_insert_that_wrote_nothing_is_skipped`, `a_retry_that_wrote_nothing_reports_skipped_in_the_retry_phase`,
`a_retry_that_fails_malformed_is_not_busy`, `an_io_fault_reports_the_io_phase`,
`any_other_database_error_stops_before_the_retry`, `a_policy_can_dispose_and_surface_a_message`,
`a_policy_can_propagate_the_original_error`), the `fault::tests` pair, and the
five `security_events::migration::tests` that v1 covered only indirectly.

## security_events/test_sqlite_reader.py (44) → `asc-sqlite-kernel` + `asc-persistence-sqlite`

| v1 case | Mode | v2 test |
|---|---|---|
| `write_read_roundtrip` | 1:1 | `security_events_store::a_written_event_round_trips_through_a_read_only_source` |
| `malformed_details_are_skipped` | 1:1 | `security_events_store::a_malformed_stored_row_is_skipped_not_fatal` |
| `query_returns_all_events` | merged | `security_events_store::a_written_event_round_trips_through_a_read_only_source` |
| `query_filter_by_event_type` | merged | `security_events_store::filters_are_applied_in_sql_including_verdict` |
| `query_filter_by_category` | merged | `security_events_store::filters_are_applied_in_sql_including_verdict` |
| `query_filter_by_trace_id` | merged | `security_events_store::filters_are_applied_in_sql_including_verdict` |
| `query_time_range_since_until` | 1:1 | `security_events_store::a_time_window_uses_an_inclusive_lower_and_exclusive_upper_bound` |
| `query_ordering_desc` | merged | `security_events_store::a_written_event_round_trips_through_a_read_only_source` |
| `query_limit_offset` | merged | `security_events_store::an_offset_count_reports_the_remainder` |
| `count_returns_total` | merged | `security_events_store::an_offset_count_reports_the_remainder` |
| `count_with_filters` | merged | `security_events_store::filters_are_applied_in_sql_including_verdict` |
| `count_filter_by_trace_id` | merged | `security_events_store::filters_are_applied_in_sql_including_verdict` |
| `count_respects_offset` | 1:1 | `security_events_store::an_offset_count_reports_the_remainder` |
| `count_by_respects_offset` | merged | `security_events_store::an_offset_count_reports_the_remainder` |
| `summary_returns_aggregates_and_latest_events` | 1:1 | `security_events_store::summary_aggregates_five_groups_and_the_latest_rows` |
| `query_rejects_naive_time_range_at_repository_boundary` | moved | `asc-security-events timestamp::tests::utc_iso_to_epoch_rejects_naive_input`, `utc_iso_to_epoch_rejects_non_utc_offset` |
| `query_count_and_count_by_support_dashboard_filters` | merged | `security_events_store::filters_are_applied_in_sql_including_verdict` |
| `count_by_category` | merged | `security_events_store::summary_aggregates_five_groups_and_the_latest_rows` |
| `count_by_event_type` | merged | `security_events_store::summary_aggregates_five_groups_and_the_latest_rows` |
| `count_by_filter_by_trace_id` | merged | `security_events_store::a_filtered_summary_repeats_the_bound_parameters_per_branch` |
| `count_by_filter_by_event_type_and_category` | merged | `security_events_store::a_filtered_summary_repeats_the_bound_parameters_per_branch` |
| `count_by_invalid_field_raises` | split | `security_events_store::count_by_rejects_a_field_outside_the_allowlist`, `error::tests::invalid_column_message_matches_v1` |
| `missing_db_returns_empty` | 1:1 | `security_events::reader::tests::a_reader_over_a_missing_database_returns_empty_results` |
| `reader_reopens_after_db_file_replaced` | 1:1 | `store::tests::read_only_store_reopens_when_the_file_is_replaced` |
| `reader_recovers_after_schema_created_in_existing_db` | merged | `store::tests::read_only_store_does_not_migrate_an_unready_schema` |
| `tilde_path_is_normalized_for_reader_and_writer` | 1:1 | `path::tests::expands_a_leading_tilde` |
| `compatibility_helpers_delegate_to_store` | by construction | `security_events::reader::tests::the_defaults_match_v1` — v2's reader owns its store directly; there is no parallel set of module-level helper functions to keep in sync. |
| `round_trips_new_tracing_fields` | merged | `security_events_store::a_written_event_round_trips_through_a_read_only_source` |
| `read_only_v1_schema_missing_new_columns_warns_and_returns_empty` | 1:1 | `store::tests::read_only_store_does_not_migrate_an_unready_schema` |
| `close_disposes_readonly_store` | 1:1 | `security_events::reader::tests::a_reader_sees_what_the_writer_wrote` (closes, then asserts the reopen) |
| `candidates_match_session_run_tool_call_and_category` | merged | `security_events_store::correlation_candidates_are_ordered_and_capped_by_the_filters` |
| `candidates_filter_inclusive_epoch_window` | merged | `security_events_store::correlation_candidates_are_ordered_and_capped_by_the_filters` |
| `candidates_filter_multiple_tool_call_ids` | merged | `security_events_store::correlation_candidates_are_ordered_and_capped_by_the_filters` |
| `candidates_are_limited_to_1000_rows` | merged | `security_events_store::correlation_candidates_are_ordered_and_capped_by_the_filters` |
| `candidates_do_not_filter_run_when_run_id_omitted` | merged | `security_events_store::correlation_candidates_are_ordered_and_capped_by_the_filters` |
| `candidates_return_empty_when_schema_unavailable` | 1:1 | `security_events_store::a_missing_database_degrades_to_empty_reads` |
| `query_filter_by_verdict` | merged | `security_events_store::filters_are_applied_in_sql_including_verdict` |
| `query_verdict_with_pagination` | merged | `security_events_store::an_offset_count_reports_the_remainder` |
| `count_with_verdict_filter` | merged | `security_events_store::filters_are_applied_in_sql_including_verdict` |
| `count_by_verdict` | 1:1 | `security_events_store::the_verdict_column_is_derived_from_both_details_shapes` |
| `count_by_verdict_with_category_filter` | merged | `security_events_store::a_filtered_summary_repeats_the_bound_parameters_per_branch` |
| `count_by_category_with_verdict_filter` | merged | `security_events_store::a_filtered_summary_repeats_the_bound_parameters_per_branch` |
| `query_without_verdict_unchanged` | merged | `security_events_store::a_written_event_round_trips_through_a_read_only_source` |
| `new_skill_ledger_events_populate_existing_verdict_index` | 1:1 | `security_events_store::the_verdict_column_is_derived_from_both_details_shapes` |

The heavy merging here is v1's own repetition: 22 of these 44 cases re-run one
filter or one grouping with a different column. v2 asserts the whole filter set
and the whole grouping set in one case each, and adds what v1 never checked — the
seven `security_events::repository::tests` covering placeholder renumbering, which
is where a hand-written SQL builder actually breaks.

Beyond v1: `security_events_store::get_returns_one_event_or_nothing`,
`a_row_whose_result_is_out_of_range_is_skipped`, the seven repository placeholder
tests, and `security_events::reader::tests::an_invalid_group_field_degrades_to_empty`.

## security_events/test_writer.py (39) → `asc-event-log`

| v1 case | Mode | v2 test |
|---|---|---|
| `write_appends_security_event_jsonl_line` | 1:1 | `security_event_writer_round_trips_through_jsonl` |
| `write_appends_multiple_security_events` | 1:1 | `jsonl::tests::appends_one_line_per_record` |
| `write_keeps_generic_jsonl_writer_contract` | 1:1 | `jsonl::tests::accessors_report_configuration` |
| `generic_writer_appends_json_serializable_records` | merged | `jsonl::tests::appends_one_line_per_record` |
| `parent_directory_created_only_once_per_writer` | merged | `jsonl::tests::creates_parent_directory_and_private_files` |
| `streams_use_independent_lock_files` | split | `jsonl::tests::lock_path_is_the_log_name_plus_lock`, `both_streams_rotate_with_mutually_recognizable_backup_names` |
| `data_and_lock_files_are_created_owner_only` | 1:1 | `jsonl::tests::creates_parent_directory_and_private_files` |
| `owner_only_mode_does_not_depend_on_process_umask` | by construction | `jsonl::tests::creates_parent_directory_and_private_files` — v2 always follows the create with an explicit `fchmod`, so the resulting mode cannot depend on the umask. A test that mutates the umask would corrupt every other test in the same process. |
| `existing_owned_data_and_lock_files_are_tightened` | 1:1 | `jsonl::tests::tightens_a_previously_loose_log_file` |
| `existing_retained_backups_are_tightened_without_rotation` | 1:1 | `jsonl::tests::tightens_pre_existing_backups_on_first_write` |
| `retained_backup_migration_does_not_follow_symlinks` | 1:1 | `jsonl::tests::a_backup_shaped_symlink_is_left_alone_together_with_its_target` |
| `retained_backup_migration_ignores_non_backup_files` | split | `jsonl::tests::backup_suffix_matcher_mirrors_the_v1_regex`, `tightens_pre_existing_backups_on_first_write` |
| `generic_writer_uses_stream_specific_rotation_state` | 1:1 | `both_streams_rotate_with_mutually_recognizable_backup_names` |
| `rotation_detection` | merged | `jsonl::tests::rotation_triggers_on_reaching_the_limit_not_exceeding_it` |
| `auto_rotation_on_size_limit` | split | `jsonl::tests::rotation_triggers_on_reaching_the_limit_not_exceeding_it`, `no_rotation_below_the_limit` |
| `rotation_tightens_legacy_file_before_moving_it` | 1:1 | `jsonl::tests::tightens_a_previously_loose_log_file` |
| `backup_count_limit` | 1:1 | `jsonl::tests::prunes_backups_beyond_the_retained_count` |
| `rotation_preserves_events` | merged | `jsonl::tests::rotated_backups_keep_the_v1_name_shape_and_mode` |
| `timestamp_format_in_backup_filename` | 1:1 | `jsonl::tests::rotated_backups_keep_the_v1_name_shape_and_mode` |
| `oldest_backups_are_deleted` | 1:1 | `jsonl::tests::pruning_orders_by_mtime_not_by_name` |
| `cleanup_preserves_most_recent_backups` | merged | `jsonl::tests::prunes_backups_beyond_the_retained_count` |
| `cleanup_detailed_verification` | merged | `jsonl::tests::prunes_backups_beyond_the_retained_count` |
| `collision_guard_backups_are_recognized` | 1:1 | `jsonl::tests::same_millisecond_collisions_get_a_counter_suffix` |
| `non_backup_files_are_not_deleted` | 1:1 | `jsonl::tests::backup_suffix_matcher_mirrors_the_v1_regex` |
| `mixed_cleanup_respects_backup_count` | merged | `jsonl::tests::prunes_backups_beyond_the_retained_count` |
| `cross_process_concurrent_writes_no_rotation` | moved | `jsonl::tests::concurrent_writers_do_not_lose_lines` |
| `cross_process_rotation_under_contention` | moved | `jsonl::tests::concurrent_writers_do_not_lose_lines` |
| `new_events_land_in_current_file_after_rotation` | merged | `jsonl::tests::rotation_triggers_on_reaching_the_limit_not_exceeding_it` |
| `flock_loser_reopens_and_writes` | merged | `jsonl::tests::concurrent_writers_do_not_lose_lines` |
| `cross_process_events_carry_distinct_pids` | moved | `asc-security-events event::tests::defaults_match_v1` — the pid is stamped per event at construction, not cached by the writer. |
| `write_with_no_fd_does_not_raise` | 1:1 | `jsonl::tests::write_swallows_failures_and_notifies` |
| `write_serialization_failure_does_not_emit_stderr` | 1:1 | `jsonl::tests::write_swallows_serialization_failures` |
| `write_or_raise_surfaces_serialization_failure_without_stderr` | split | `jsonl::tests::write_swallows_serialization_failures`, `observability_writer_surfaces_failures` |
| `write_invokes_on_error_for_io_failure` | 1:1 | `jsonl::tests::write_swallows_failures_and_notifies` |
| `write_invokes_on_error_for_rotation_failure` | 1:1 | `jsonl::tests::a_rotation_that_cannot_rename_reports_and_still_appends` |
| `cleanup_invokes_on_error_for_unlink_failure` | merged | `jsonl::tests::a_rotation_that_cannot_rename_reports_and_still_appends` — same `notify_error` route. The unlink branch has no separate test because the only way to make `unlink` fail is an unwritable directory, which already stops the rename that precedes it. |
| `write_swallows_on_error_callback_failure` | merged | `security_event_writer_never_fails_the_caller` |
| `write_without_on_error_remains_silent` | merged | `security_event_writer_never_fails_the_caller` |
| `concurrent_writes` | 1:1 | `jsonl::tests::concurrent_writers_do_not_lose_lines` |

v1's cross-process cases spawn real subprocesses through `pytest`'s
`tmp_path`; v2 uses threads in one process. The mechanism under test — an
advisory `flock` on a dedicated lock file — is process-scoped either way, and the
`asc-event-log` integration tests additionally read files that v1 wrote.

Beyond v1: `error::tests::io_errors_report_path_and_operation_without_payload`
(a failure diagnostic must never echo the event being logged),
`jsonl::tests::defaults_match_v1`, `non_ascii_is_not_escaped`, and the four v1
interop tests (`v1_lines_survive_a_v2_round_trip`,
`v1_security_event_lines_parse_with_every_field_preserved`,
`v1_observability_lines_parse_with_every_field_preserved`,
`v2_written_lines_are_exported_for_the_v1_side`).

## security_events/test_summary_formatter.py (70) → `asc-security-summary`

| v1 case | Mode | v2 test |
|---|---|---|
| `empty_list_returns_guidance_message` | 1:1 | `tests::an_empty_event_list_renders_the_placeholder` |
| `compliance_includes_fixed_count` | merged | `sections::tests::reinforce_fixes_count_towards_compliance_and_hide_the_hint` |
| `scan_count_and_compliance` | 1:1 | `sections::tests::a_hardening_section_reports_scans_and_compliance` |
| `reinforcement_count` | merged | `sections::tests::reinforce_fixes_count_towards_compliance_and_hide_the_hint` |
| `failed_scan` | 1:1 | `sections::tests::a_hardening_section_without_stats_shows_the_latest_error` |
| `failed_scan_with_stats_still_shows_compliance` | split | `sections::tests::a_hardening_section_reports_scans_and_compliance`, `details::tests::a_failed_event_verifies_as_failed_regardless_of_counters` |
| `failed_reinforce_with_stats_contributes_fixed_count` | merged | `sections::tests::reinforce_fixes_count_towards_compliance_and_hide_the_hint` |
| `successful_verifications` | 1:1 | `sections::tests::an_asset_verify_section_reports_each_outcome` |
| `failed_verification` | 1:1 | `sections::tests::a_failed_verification_surfaces_the_error_and_the_hint` |
| `single_skill_verify_after_full_verify` | split | `details::tests::a_full_verify_is_one_without_a_named_skill`, `posture::tests::a_single_skill_verification_never_changes_the_posture` |
| `no_candidates_is_reported_as_not_assessed` | 1:1 | `sections::tests::a_skipped_verification_is_reported_as_not_assessed` |
| `legacy_zero_counts_are_reported_as_not_assessed` | 1:1 | `details::tests::a_legacy_outcome_is_reconstructed_from_the_counters` |
| `latest_no_candidates_does_not_clear_prior_full_failure` | 1:1 | `posture::tests::a_no_candidate_run_is_skipped_in_favour_of_the_next_conclusive_one` |
| `nested_no_candidates_overrides_failed_event_envelope` | merged | `details::tests::an_explicit_outcome_wins_over_the_legacy_reconstruction` |
| `nested_failed_outcome_overrides_successful_event_envelope` | merged | `details::tests::an_explicit_outcome_wins_over_the_legacy_reconstruction` |
| `deny_findings` | merged | `sections::tests::a_code_scan_section_counts_verdicts_alphabetically` |
| `pass_verdict_no_deny_section` | merged | `sections::tests::a_code_scan_section_counts_verdicts_alphabetically` |
| `mixed_verdicts_breakdown` | merged | `sections::tests::a_code_scan_section_counts_verdicts_alphabetically` |
| `sandbox_interventions` | 1:1 | `sections::tests::a_sandbox_section_is_a_single_counter` |
| `section_header_present` (code scan) | merged | `tests::a_single_category_renders_header_section_and_footer` |
| `scan_count_succeeded` | merged | `sections::tests::a_code_scan_section_counts_verdicts_alphabetically` |
| `scan_count_with_failed_event` | 1:1 | `sections::tests::a_code_scan_section_omits_the_verdict_line_when_all_failed` |
| `verdict_breakdown_pass_only` | merged | `sections::tests::a_code_scan_section_counts_verdicts_alphabetically` |
| `verdict_breakdown_mixed` | merged | `sections::tests::a_code_scan_section_counts_verdicts_alphabetically` |
| `threat_type_breakdown` | merged | `sections::tests::a_prompt_scan_section_caps_the_threat_list_at_three` |
| `no_threat_type_section_when_all_pass` | merged | `sections::tests::a_clean_prompt_scan_has_no_threat_lines` |
| `latest_threats_shown` | split | `sections::tests::a_prompt_scan_section_caps_the_threat_list_at_three`, `a_single_prompt_threat_uses_the_singular_heading` |
| `at_most_3_latest_threats_shown` | 1:1 | `sections::tests::a_prompt_scan_section_caps_the_threat_list_at_three` |
| `no_latest_threats_section_when_all_pass` | merged | `sections::tests::a_clean_prompt_scan_has_no_threat_lines` |
| `good_status` | 1:1 | `posture::tests::a_quiet_window_reads_as_good` |
| `sandbox_block_does_not_affect_posture` | by construction | `tests::a_single_category_renders_header_section_and_footer` — renders a sandbox-only report and asserts `Good`; `sandbox` is not a field of `PostureInput`. |
| `critical_on_harden_failure` | 1:1 | `posture::tests::a_failed_hardening_run_needs_attention` |
| `code_deny_does_not_affect_posture` | 1:1 | `tests::a_denied_code_scan_is_reported_without_raising_the_header` |
| `prompt_scan_warn_does_not_affect_posture` | 1:1 | `posture::tests::a_warned_scan_does_not_need_attention` |
| `prompt_scan_deny_triggers_needs_attention` | 1:1 | `posture::tests::a_denied_scan_of_either_kind_needs_attention` |
| `prompt_scan_deny_failed_event_does_not_affect_posture` | 1:1 | `posture::tests::a_failed_scan_does_not_raise_even_with_a_deny_verdict` |
| `asc_order_events_still_pick_latest` | 1:1 | `posture::tests::only_the_newest_hardening_run_counts` |
| `verify_event_failed_triggers_needs_attention` | merged | `posture::tests::a_no_candidate_run_is_skipped_in_favour_of_the_next_conclusive_one` |
| `no_suggestion_when_latest_harden_failed` | merged | `posture::tests::a_failed_hardening_run_without_stats_suggests_nothing` |
| `no_reinforce_suggestion_when_failed_harden_has_parser_failure` | 1:1 | `posture::tests::a_failed_hardening_run_without_stats_suggests_nothing` |
| `no_suggestion_when_no_hardening_events` | merged | `posture::tests::a_passing_ledger_suggests_nothing` |
| `system_status_good_prefix` | merged | `posture::tests::a_quiet_window_reads_as_good` |
| `system_status_needs_attention_prefix` | merged | `posture::tests::a_failed_hardening_run_needs_attention` |
| `verdict_prefix_in_code_scan` | merged | `sections::tests::a_code_scan_section_counts_verdicts_alphabetically` |
| `combined_report_structure` | 1:1 | `tests::sections_follow_the_declared_order_regardless_of_input_order` |
| `combined_with_prompt_scan` | merged | `tests::sections_follow_the_declared_order_regardless_of_input_order` |
| `footer_stats` | split | `posture::tests::a_footer_reports_totals_and_the_newest_age`, `tests::the_injected_clock_is_the_only_source_of_the_footer_age` |
| `suggested_action_for_failed_rules` | 1:1 | `posture::tests::suggestions_are_emitted_in_a_fixed_order` |
| `needs_attention_on_latest_harden_failures` | 1:1 | `posture::tests::a_succeeded_hardening_run_with_failures_still_needs_attention` |
| `needs_attention_on_latest_verify_failures` | merged | `posture::tests::a_no_candidate_run_is_skipped_in_favour_of_the_next_conclusive_one` |
| `section_header_present` (skill ledger) | merged | `sections::tests::a_skill_ledger_section_deduplicates_to_the_latest_check_per_skill` |
| `check_counts` | merged | `sections::tests::a_skill_ledger_section_deduplicates_to_the_latest_check_per_skill` |
| `certification_counts` | 1:1 | `sections::tests::certifications_report_the_verdict_with_a_scan_status_fallback` |
| `certification_counts_accept_event_verdict` | merged | `sections::tests::certifications_report_the_verdict_with_a_scan_status_fallback` |
| `status_distribution` | 1:1 | `sections::tests::the_latest_statuses_helper_matches_the_section` |
| `tampered_alert` | merged | `sections::tests::a_denied_skill_gets_its_own_block` |
| `denied_alert` | 1:1 | `sections::tests::a_denied_skill_gets_its_own_block` |
| `deduplicates_to_latest_per_skill` | 1:1 | `sections::tests::a_skill_ledger_section_deduplicates_to_the_latest_check_per_skill` |
| `no_skills_tracked_when_only_failed_checks` | split | `sections::tests::a_failed_check_never_contributes_a_status`, `a_check_without_a_skill_directory_is_not_tracked` |
| `tampered_triggers_needs_attention` | merged | `posture::tests::a_tampered_or_denied_skill_needs_attention` |
| `deny_triggers_needs_attention` | merged | `posture::tests::a_tampered_or_denied_skill_needs_attention` |
| `pass_does_not_trigger_needs_attention` | merged | `posture::tests::a_tampered_or_denied_skill_needs_attention` |
| `drifted_does_not_trigger_needs_attention` | merged | `posture::tests::a_tampered_or_denied_skill_needs_attention` |
| `failed_event_does_not_affect_posture` | 1:1 | `sections::tests::a_failed_check_never_contributes_a_status` |
| `tampered_suggestion` | merged | `posture::tests::suggestions_are_emitted_in_a_fixed_order` |
| `drifted_suggestion` | merged | `posture::tests::suggestions_are_emitted_in_a_fixed_order` |
| `none_suggestion` | merged | `posture::tests::suggestions_are_emitted_in_a_fixed_order` |
| `no_suggestion_when_all_pass` | 1:1 | `posture::tests::a_passing_ledger_suggests_nothing` |
| `section_order_in_combined_report` | 1:1 | `tests::sections_follow_the_declared_order_regardless_of_input_order` |
| `pii_scan_section_and_deny_posture` | split | `sections::tests::a_pii_section_sums_finding_types_across_events`, `posture::tests::a_denied_scan_of_either_kind_needs_attention` |

Beyond v1: the twelve `details::tests` that pin how a `details` blob is read
(mode priority, truthiness of an empty string, non-object nested fields,
Python's spelling of `None` / `True` / `False`), plus
`sections::tests::a_hardening_event_in_neither_mode_is_counted_nowhere`,
`a_pii_section_tolerates_a_non_object_summary`,
`tests::each_category_is_ordered_newest_first`, and
`tests::an_unknown_category_contributes_only_to_the_footer`.

## observability/test_schema.py (36) → `asc-observability`

| v1 case | Mode | v2 test |
|---|---|---|
| `minimal_metric_examples_cover_each_hook` | 1:1 | `metrics::tests::allowlist_covers_every_hook` |
| `each_hook_accepts_minimal_allowed_metric` | 1:1 | `hook::tests::metric_counts_match_v1` |
| `camel_case_payload_dumps_back_to_wire_aliases` | split | `record::tests::wire_form_matches_v1_golden_lines`, `snake_case_aliases_are_accepted` |
| `observability_metadata_truncates_long_correlation_ids_with_suffix` | split | `record::tests::caps_correlation_ids`, `correlation::tests::long_ids_are_capped_to_v1_shape`, `capping_counts_characters_not_bytes`, `short_ids_pass_through` |
| `all_allowed_metrics_are_not_required` | merged | `record::tests::metrics_iterate_in_declaration_order` |
| `before_agent_run_accepts_run_start_metrics` | merged | `hook::tests::metadata_shapes_match_v1_record_classes` |
| `before_agent_run_accepts_input_context_metrics` | merged | `hook::tests::metric_counts_match_v1` |
| `before_llm_call_accepts_complete_model_call_metrics` | merged | `hook::tests::metric_counts_match_v1` |
| `after_llm_call_accepts_model_call_ended_metrics` | merged | `hook::tests::metric_counts_match_v1` |
| `after_agent_run_accepts_llm_output_response` | merged | `hook::tests::metric_counts_match_v1` |
| `after_agent_run_accepts_llm_output_tool_use_summary` | merged | `hook::tests::metric_counts_match_v1` |
| `after_llm_call_accepts_llm_output_response_without_call_id` | merged | `record::tests::drops_metadata_fields_the_hook_does_not_model` |
| `after_llm_call_accepts_llm_output_tool_use_summary_without_call_id` | merged | `record::tests::drops_metadata_fields_the_hook_does_not_model` |
| `after_llm_call_drops_unsupported_response_detail_metrics` | 1:1 | `record::tests::rejects_empty_and_unknown_only_metrics` |
| `tool_call_records_dump_tool_call_id` | 1:1 | `hook::tests::metadata_shapes_match_v1_record_classes` |
| `before_tool_call_accepts_pii_input_hash_metric` | merged | `hook::tests::metric_counts_match_v1` |
| `after_tool_call_accepts_query_friendly_result_metrics` | merged | `hook::tests::metric_counts_match_v1` |
| `after_agent_run_accepts_final_summary_metrics` | merged | `hook::tests::metric_counts_match_v1` |
| `before_agent_run_accepts_assembled_input_metrics` | merged | `hook::tests::metric_counts_match_v1` |
| `before_agent_run_accepts_input_records_without_call_id` | merged | `record::tests::drops_metadata_fields_the_hook_does_not_model` |
| `after_llm_call_accepts_missing_call_id` | merged | `record::tests::drops_metadata_fields_the_hook_does_not_model` |
| `tool_call_metadata_requires_tool_call_id` | 1:1 | `record::tests::rejects_tool_call_hook_without_tool_call_id` |
| `common_metadata_requires_session_id_and_run_id` | 1:1 | `record::tests::rejects_missing_session_and_run` |
| `empty_session_id_or_run_id_is_allowed` | merged | `record::tests::rejects_missing_session_and_run` |
| `invalid_payload_values_fail_validation` | 1:1 | `record::tests::rejects_non_object_members` |
| `after_tool_call_uses_duration_ms` | merged | `hook::tests::metric_names_are_unique_per_hook` |
| `unknown_hook_fails` | split | `record::tests::rejects_unknown_hook_with_v1_wording`, `error::tests::unknown_hook_message_matches_v1`, `metrics::tests::unknown_hook_has_no_allowed_metrics` |
| `before_context_assembly_is_not_supported` | merged | `hook::tests::hook_names_round_trip` |
| `after_llm_response_is_not_supported` | merged | `hook::tests::hook_names_round_trip` |
| `deprecated_metrics_are_dropped_and_rejected_when_empty` | 1:1 | `record::tests::rejects_empty_and_unknown_only_metrics` |
| `unknown_metric_is_dropped_when_supported_metrics_remain` | merged | `record::tests::rejects_empty_and_unknown_only_metrics` |
| `only_unknown_metrics_fails` | merged | `record::tests::rejects_empty_and_unknown_only_metrics` |
| `extra_top_level_and_metadata_fields_are_dropped` | 1:1 | `record::tests::drops_metadata_fields_the_hook_does_not_model` |
| `empty_metrics_fails` | merged | `record::tests::rejects_empty_and_unknown_only_metrics` |
| `invalid_timestamp_fails` | 1:1 | `record::tests::rejects_unparseable_timestamp` |
| `naive_timestamp_fails` | 1:1 | `record::tests::rejects_naive_timestamp` |

Beyond v1: `record::tests::epoch_matches_v1_timestamp`,
`sub_microsecond_precision_is_truncated_like_python`, `round_trips_through_json`,
`column_json_helpers_match_wire_members`, `direct_construction_validates_the_same_way`,
`hook::tests::sorted_names_match_v1_error_ordering`,
`config::tests` (2), `schema_version::tests::schema_version_matches_v1`, and the
two `summary::tests`.

## observability/test_writer.py (16) → `asc-event-log` + `asc-persistence-sqlite`

| v1 case | Mode | v2 test |
|---|---|---|
| `observability_package_import_does_not_load_sqlalchemy` | merged | `asc-event-sink tests::merely_linking_this_crate_initializes_nothing` |
| `observability_jsonl_writer_only_writes_jsonl` | 1:1 | `asc-event-log observability_writer_round_trips_through_jsonl` |
| `observability_sqlite_writer_only_writes_independent_sqlite_index` | 1:1 | `observability::writer::tests::both_entry_points_persist_a_record` |
| `observability_sqlite_write_or_raise_surfaces_skipped_insert` | 1:1 | `observability_store::a_write_to_a_disabled_stream_surfaces_the_v1_message` |
| `observability_sqlite_write_or_raise_reports_busy_without_dispose` | 1:1 | `observability::policy::tests::a_busy_corruption_retry_keeps_the_connection` |
| `observability_repository_insert_returns_false_for_validation_error_without_dispose` | merged | `observability::policy::tests::a_malformed_record_propagates_without_disposing` |
| `observability_sqlite_write_or_raise_propagates_validation_errors_without_dispose` | 1:1 | `observability_store::a_malformed_record_surfaces_without_disposing_the_connection` |
| `observability_sqlite_write_or_raise_disposes_on_io_error` | 1:1 | `observability::policy::tests::an_io_fault_disposes_and_propagates` |
| `observability_sqlite_write_or_raise_disposes_on_corruption_retry_error` | 1:1 | `observability::policy::tests::a_failed_corruption_retry_disposes_only_for_a_database_fault` |
| `observability_sqlite_columns_are_core_index_and_correlation_only` | split | `observability::table::tests::column_order_matches_v1`, `there_are_no_convergent_columns`, `there_is_exactly_one_table` |
| `observability_sqlite_writer_prunes_on_close_not_write` | 1:1 | `observability_store::close_runs_retention_through_the_maintenance_gate` |
| `observability_sqlite_writer_closes_through_maintenance_gate` | merged | `observability_store::close_runs_retention_through_the_maintenance_gate` |
| `observability_sqlite_writer_uses_schema_version_fast_path` | 1:1 | `v1_fixtures::an_observability_fixture_keeps_its_only_revision` |
| `record_observability_dual_writes_jsonl_and_sqlite` | 1:1 | `asc-event-sink observability::tests::both_paths_receive_the_record` |
| `observability_writer_indexes_llm_call_correlation_only` | merged | `observability_store::the_correlation_columns_follow_the_hook_metadata_shape` |
| `observability_writer_indexes_tool_call_correlation_only` | merged | `observability_store::the_correlation_columns_follow_the_hook_metadata_shape` |

Beyond v1: `observability::policy::tests::every_fault_surfaces`,
`schema_drift_propagates_without_disposing`,
`the_post_rebuild_disabled_check_reuses_the_disabled_message`,
`the_three_messages_match_v1_verbatim`,
`observability::writer::tests::the_defaults_match_v1`,
`write_swallows_what_write_or_raise_surfaces`,
`a_failing_retention_pass_is_swallowed_and_still_marks_the_gate`,
and `asc-event-log observability_writer_defaults_match_v1`.

## observability/test_repository_read.py (13) → `asc-persistence-sqlite`

| v1 case | Mode | v2 test |
|---|---|---|
| `list_sessions_empty_db_returns_empty` | merged | `observability_store::a_missing_database_degrades_to_empty_reads` |
| `list_sessions_orders_by_last_seen_desc` | 1:1 | `observability_store::sessions_are_listed_most_recent_first_with_their_aggregates` |
| `list_runs_preview_fallback_and_truncation` | 1:1 | `observability_store::the_preview_falls_back_to_prompt_and_is_truncated` |
| `list_runs_preview_query_selects_first_before_agent_run_per_run_in_sql` | 1:1 | `observability_store::the_preview_comes_from_the_first_before_agent_run_of_the_run` |
| `list_runs_preview_tolerates_malformed_metrics_json` | 1:1 | `observability_store::a_run_without_a_before_agent_run_row_has_no_preview` |
| `list_runs_nonexistent_session_returns_empty` | merged | `observability_store::a_missing_database_degrades_to_empty_reads` |
| `list_events_ordered_with_fields_preserved` | split | `observability_store::events_of_one_run_come_back_oldest_first`, `a_written_record_round_trips_with_its_wire_fields_intact` |
| `list_events_is_scoped_to_session_when_run_ids_collide` | 1:1 | `observability_store::listing_events_is_scoped_to_the_session_when_run_ids_collide` |
| `list_events_nonexistent_run_returns_empty` | merged | `observability_store::a_missing_database_degrades_to_empty_reads` |
| `reader_can_be_reopened` | 1:1 | `observability::reader::tests::a_reader_sees_what_the_writer_wrote` |
| `read_during_concurrent_write` | 1:1 | `observability_store::a_reader_keeps_up_with_a_writer_that_is_still_appending` |
| `repository_read_methods_return_empty_without_session_factory` | merged | `observability_store::a_missing_database_degrades_to_empty_reads` |
| `repository_read_methods_dispose_and_return_empty_on_sqlalchemy_error` | moved | `asc-sqlite-kernel query::tests::a_failing_query_degrades_to_the_default` |

Beyond v1: `observability_store::counts_cover_records_sessions_and_runs`,
`a_time_window_bounds_counts_inclusively_then_exclusively`,
`paging_applies_limit_and_offset_independently`,
`the_converged_schema_carries_every_declared_column_and_index`.

## observability/test_sqlite_reader.py (3) → `asc-persistence-sqlite`

| v1 case | Mode | v2 test |
|---|---|---|
| `observability_reader_lists_sessions_runs_and_events` | 1:1 | `observability::reader::tests::a_reader_sees_what_the_writer_wrote` |
| `observability_reader_counts_sessions_and_runs` | 1:1 | `observability_store::counts_cover_records_sessions_and_runs` |
| `observability_reader_close_disposes_store` | merged | `observability::reader::tests::a_reader_sees_what_the_writer_wrote` (closes, then asserts the reopen) |

Beyond v1: `observability::reader::tests::a_reader_over_a_missing_database_returns_empty_results`,
`the_table_spec_is_a_required_constructor_argument`.

## observability/test_retention.py (2) → `asc-persistence-sqlite`

| v1 case | Mode | v2 test |
|---|---|---|
| `retention_prunes_by_observed_at_epoch` | 1:1 | `observability_store::retention_prunes_by_observed_at_epoch` |
| `observability_prune_disposes_store_on_sqlalchemy_error` | moved | `observability::writer::tests::a_failing_retention_pass_is_swallowed_and_still_marks_the_gate` — v2 does not dispose; see D-25. |

## Gaps closed while writing this ledger

Building the mapping surfaced nine v1 behaviours with no v2 counterpart. All nine
were closed by writing the missing test rather than by recording a deprecation:

| Behaviour | v2 test added |
|---|---|
| Ten threads sharing one writer lose nothing | `security_events_store::ten_threads_sharing_one_sink_lose_no_rows` |
| Independent writers lose rows only to `SQLITE_BUSY` | `security_events_store::independent_sinks_on_one_database_only_lose_rows_to_busy` |
| A cold bootstrap race leaves the database usable | `security_events_store::a_cold_bootstrap_race_keeps_the_database_usable` |
| A backup-shaped symlink is not chmod'ed through | `jsonl::tests::a_backup_shaped_symlink_is_left_alone_together_with_its_target` |
| A failed rotation is reported, not swallowed | `jsonl::tests::a_rotation_that_cannot_rename_reports_and_still_appends` |
| Sessions are isolated when run ids collide | `observability_store::listing_events_is_scoped_to_the_session_when_run_ids_collide` |
| A reader keeps up with an appending writer | `observability_store::a_reader_keeps_up_with_a_writer_that_is_still_appending` |
| A denied code scan does not raise the posture | `tests::a_denied_code_scan_is_reported_without_raising_the_header` |
| A failed scan event never raises the posture | `posture::tests::a_failed_scan_does_not_raise_even_with_a_deny_verdict` |
| A failing retention pass stays silent (×2 streams) | `security_events::writer::tests::a_failing_retention_pass_is_swallowed_and_still_marks_the_gate`, and the observability twin |

Two of these were more than test gaps. `asc-event-log` was silently discarding a
failed rename and a failed backup unlink; both now route through
`notify_error`, matching v1 and the repository rule that errors stay visible.

## Out of scope

These v1 test files exercise callers of the database layer, not the layer
itself, and are not part of this migration:

| File | Cases | Reason |
|---|---|---|
| `observability/test_correlation.py` | 25 | `correlation.py` is a consumer of the reader. |
| `test_cli.py` | 26 | CLI argument handling and rendering. |
| `test_review.py` | 21 | Textual TUI. |
| `test_session_report.py` | 8 | Report composition on top of the reader. |
| hooks (`qoder`, `qwen`, `cosh`, `codex`, `hermes`), `telemetry/`, `daemon/test_security_query_handler.py`, `e2e/cli/`, skill-ledger suites | — | All call `log_event` / `record_observability`; the sink contract they rely on is covered by `asc-event-sink`. |
