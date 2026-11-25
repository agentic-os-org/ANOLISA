use super::*;
use tokenless_ccr::{InMemoryStore, StashError, StashWrite};

/// P2-1 review regression: a stats-recording warning must not be
/// able to fail the command when stderr itself cannot be written
/// (a full filesystem behind redirected logs). `eprintln!` panics
/// on that shape, turning the fail-soft warning into exit 101 while
/// the compression output is fine; `warn_soft` must survive it.
/// Redirect fd 2 to /dev/full, call the helper, restore, and assert
/// the call returned — a panicking write makes this test fail.
#[test]
#[cfg(target_os = "linux")]
fn warn_soft_survives_an_unwritable_stderr() {
    use std::os::unix::io::AsRawFd;

    let dev_full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    let saved = unsafe { libc::dup(2) };
    assert!(saved >= 0, "dup stderr");
    unsafe {
        libc::dup2(dev_full.as_raw_fd(), 2);
    }
    // The helper's whole contract: no panic, error discarded.
    warn_soft("[tokenless-stats] WARNING: simulated stderr failure");
    // Restore before any assert can print.
    let restored = unsafe { libc::dup2(saved, 2) };
    let _ = std::io::Write::flush(&mut std::io::stderr());
    assert!(restored >= 0, "restore stderr");
    unsafe {
        libc::close(saved);
    }
    drop(dev_full);
}

struct AlwaysFail;

impl StashStore for AlwaysFail {
    fn stash(&self, _payload: &str) -> Result<tokenless_ccr::StashWrite, StashError> {
        Err(StashError::Backend("simulated".to_string()))
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

fn long_response() -> String {
    serde_json::to_string(&serde_json::json!({
        "items": (0..100).collect::<Vec<_>>(),
        "tail": format!("RECOVERY_SENTINEL={}\n", "x".repeat(200)),
    }))
    .unwrap()
}

#[test]
fn compress_and_retrieve_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let payload = format!("RECOVERY_SENTINEL=ORCHID-7291\n{}", "世界".repeat(200));
    let input = serde_json::to_string(&serde_json::json!({ "tail": payload })).unwrap();
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: false,
        ..RuntimeConfig::default()
    })
    .unwrap();
    let result = runtime
        .compress_response(
            &input,
            &CompressOptions {
                truncate_strings_at: Some(tokenless_ccr::truncation_suffix_char_len() + 16),
                require_reversible: true,
                ..CompressOptions::default()
            },
            &Attribution::new("test"),
        )
        .unwrap();
    assert!(result.applied());
    let hash = extract_hash(&result.output).unwrap();
    let restored = runtime.retrieve(&hash.to_ascii_uppercase()).unwrap();
    assert_eq!(restored, payload);
}

#[test]
fn reversible_policy_preserves_input_without_store() {
    let input = long_response();
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            truncate_arrays_at: Some(2),
            require_reversible: true,
            ..CompressOptions::default()
        },
        true,
        None,
    )
    .unwrap();
    assert_eq!(result.disposition, Disposition::RecoverabilityUnavailable);
    assert_eq!(result.output, input);
}

#[test]
fn reversible_policy_preserves_input_after_string_stash_failure() {
    let input = serde_json::to_string(&serde_json::json!({ "tail": "x".repeat(400) })).unwrap();
    let store = Arc::new(AlwaysFail) as Arc<dyn StashStore>;
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            truncate_strings_at: Some(tokenless_ccr::truncation_suffix_char_len() + 16),
            require_reversible: true,
            ..CompressOptions::default()
        },
        true,
        Some(&store),
    )
    .unwrap();

    assert_eq!(result.disposition, Disposition::RecoverabilityUnavailable);
    assert_eq!(result.output, input);
    assert_eq!(result.stash_errors, Some(1));
    assert_eq!(result.unrecoverable_truncations, Some(1));
}

#[test]
fn reversible_policy_preserves_input_after_depth_stash_failure() {
    let input = serde_json::to_string(&serde_json::json!({
        "nested": {"payload": "x".repeat(400)}
    }))
    .unwrap();
    let store = Arc::new(AlwaysFail) as Arc<dyn StashStore>;
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            max_depth: Some(0),
            require_reversible: true,
            ..CompressOptions::default()
        },
        true,
        Some(&store),
    )
    .unwrap();

    assert_eq!(result.disposition, Disposition::RecoverabilityUnavailable);
    assert_eq!(result.output, input);
    assert_eq!(result.stash_errors, Some(1));
    assert_eq!(result.unrecoverable_truncations, Some(1));
}

#[test]
fn reversible_policy_preserves_input_when_string_marker_cannot_fit() {
    let input = serde_json::to_string(&serde_json::json!({ "tail": "x".repeat(400) })).unwrap();
    let store = Arc::new(InMemoryStore::new()) as Arc<dyn StashStore>;
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            truncate_strings_at: Some(10),
            require_reversible: true,
            ..CompressOptions::default()
        },
        true,
        Some(&store),
    )
    .unwrap();

    assert_eq!(result.disposition, Disposition::RecoverabilityUnavailable);
    assert_eq!(result.output, input);
    assert_eq!(result.stash_errors, Some(0));
    assert_eq!(result.unrecoverable_truncations, Some(1));
}

#[test]
fn cli_policy_allows_lossy_output_without_store() {
    let input = long_response();
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            truncate_arrays_at: Some(2),
            ..CompressOptions::default()
        },
        true,
        None,
    )
    .unwrap();
    assert!(result.applied());
    assert_ne!(result.output, input);
    assert!(result.output.contains("truncated"));
    assert_eq!(result.unrecoverable_truncations, None);
}

#[test]
fn explicit_response_api_preserves_top_level_nulls() {
    let mut value = serde_json::Map::new();
    value.insert(
        "keep".to_owned(),
        serde_json::Value::String("value".to_owned()),
    );
    for index in 0..80 {
        value.insert(format!("null_{index}"), serde_json::Value::Null);
    }
    let input = serde_json::to_string(&value).unwrap();

    let result =
        compress_response_with_store(&input, &CompressOptions::default(), true, None).unwrap();

    assert_eq!(result.disposition, Disposition::NoSavings);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result.output).unwrap(),
        serde_json::Value::Object(value)
    );
}

#[test]
fn dry_run_does_not_write_stash() {
    let input = long_response();
    let store = Arc::new(InMemoryStore::new()) as Arc<dyn StashStore>;
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            truncate_arrays_at: Some(2),
            ..CompressOptions::default()
        },
        false,
        Some(&store),
    )
    .unwrap();
    assert_eq!(result.disposition, Disposition::DryRun);
    assert_eq!(result.output, input);
    assert_eq!(store.len(), 0);
    assert_eq!(result.stash_writes, None);
}

#[test]
fn no_savings_returns_original() {
    let input = r#"{"value":1}"#;
    let result =
        compress_response_with_store(input, &CompressOptions::default(), true, None).unwrap();
    assert_eq!(result.disposition, Disposition::NoSavings);
    assert_eq!(result.output, input);
}

#[test]
fn no_savings_rolls_back_orphan_stash() {
    let input = r#"["a","b"]"#;
    let store = Arc::new(InMemoryStore::new()) as Arc<dyn StashStore>;
    let result = compress_response_with_store(
        input,
        &CompressOptions {
            truncate_arrays_at: Some(1),
            ..CompressOptions::default()
        },
        true,
        Some(&store),
    )
    .unwrap();
    assert_eq!(result.disposition, Disposition::NoSavings);
    assert_eq!(result.output, input);
    assert_eq!(store.len(), 0);
    assert_eq!(result.stash_writes, Some(0));
}

#[test]
fn rollback_delete_failures_surface_in_stash_errors() {
    struct StashOkDeleteFails(InMemoryStore);

    impl StashStore for StashOkDeleteFails {
        fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
            self.0.stash(payload)
        }

        fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
            self.0.retrieve(hash)
        }

        fn len(&self) -> usize {
            self.0.len()
        }

        fn evict_expired(&self) -> Result<usize, StashError> {
            self.0.evict_expired()
        }

        fn delete(&self, _hash: &str, _generation: u64) -> Result<bool, StashError> {
            Err(StashError::Backend("simulated delete failure".to_string()))
        }
    }

    // Twelve short items: the truncation marker outgrows the removed
    // tail, so the candidate is rejected as no-savings after stashing.
    let input = r#"["a","b","c","d","e","f","g","h","i","j","k","l"]"#;
    let store = Arc::new(StashOkDeleteFails(InMemoryStore::new())) as Arc<dyn StashStore>;
    let result = compress_response_with_store(
        input,
        &CompressOptions {
            truncate_arrays_at: Some(1),
            array_tail_preserve: Some(0),
            ..CompressOptions::default()
        },
        true,
        Some(&store),
    )
    .unwrap();

    assert_eq!(result.disposition, Disposition::NoSavings);
    assert_eq!(result.output, input);
    // The rollback delete failed: the orphaned row is still live and the
    // failure is visible, so the CLI's stash-health warning fires.
    assert_eq!(result.stash_writes, Some(1));
    assert_eq!(result.stash_errors, Some(1));
    assert_eq!(store.len(), 1);
}

#[test]
fn invalid_json_is_structured_error() {
    let error = compress_response_with_store("not json", &CompressOptions::default(), true, None)
        .unwrap_err();
    assert!(matches!(error, RuntimeError::InvalidJson(_)));
}

#[test]
fn explicit_json_api_compresses_string_roots() {
    // JSON scalars remain valid input to the explicit JSON API even
    // though the general PostTool detector only dispatches structured
    // JSON and wrapped structured output.
    let input = serde_json::to_string(&"x".repeat(400)).unwrap();
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            truncate_strings_at: Some(tokenless_ccr::truncation_suffix_char_len() + 16),
            ..CompressOptions::default()
        },
        true,
        None,
    )
    .unwrap();
    assert_eq!(result.disposition, Disposition::Applied);
    assert_ne!(result.output, input);
    assert!(result.after_tokens < result.before_tokens);
}

#[test]
fn compaction_is_lossless_and_passes_required_reversibility() {
    let input = serde_json::to_string_pretty(&serde_json::json!({
        "value": 1,
        "noise": null,
        "debug": "x".repeat(200),
    }))
    .unwrap();
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            require_reversible: true,
            ..CompressOptions::default()
        },
        true,
        None,
    )
    .unwrap();
    assert!(result.applied());
    assert_eq!(result.unrecoverable_truncations.unwrap_or(0), 0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result.output).unwrap(),
        serde_json::from_str::<serde_json::Value>(&input).unwrap()
    );
}

#[test]
fn dry_run_wins_over_required_reversibility() {
    let input = long_response();
    let result = compress_response_with_store(
        &input,
        &CompressOptions {
            truncate_arrays_at: Some(2),
            require_reversible: true,
            ..CompressOptions::default()
        },
        false,
        None,
    )
    .unwrap();
    assert_eq!(result.disposition, Disposition::DryRun);
    assert_eq!(result.output, input);
    assert!(result.after_tokens < result.before_tokens);
}

#[test]
fn runtime_records_attribution() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: true,
        ..RuntimeConfig::default()
    })
    .unwrap();
    let result = runtime
        .compress_response(
            &long_response(),
            &CompressOptions {
                truncate_arrays_at: Some(2),
                require_reversible: true,
                ..CompressOptions::default()
            },
            &Attribution {
                agent_id: "agentscope".to_string(),
                session_id: Some("session-a".to_string()),
                tool_use_id: Some("tool-a".to_string()),
            },
        )
        .unwrap();
    assert!(result.applied());

    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    let records = recorder.records_by_session("session-a", None).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].agent_id, "agentscope");
    assert_eq!(records[0].tool_use_id.as_deref(), Some("tool-a"));
}

#[test]
fn runtime_records_the_result_token_estimates() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: true,
        ..RuntimeConfig::default()
    })
    .unwrap();
    let input = serde_json::to_string(&serde_json::json!({
        "tail": "世界".repeat(300)
    }))
    .unwrap();
    let result = runtime
        .compress_response(
            &input,
            &CompressOptions {
                truncate_strings_at: Some(tokenless_ccr::truncation_suffix_char_len() + 16),
                require_reversible: true,
                ..CompressOptions::default()
            },
            &Attribution {
                agent_id: "agentscope".to_string(),
                session_id: Some("unicode-session".to_string()),
                tool_use_id: None,
            },
        )
        .unwrap();
    assert!(result.applied());

    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    let records = recorder
        .records_by_session("unicode-session", None)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].before_tokens, result.before_tokens);
    assert_eq!(records[0].after_tokens, result.after_tokens);
}

#[test]
fn runtime_records_json_record_reduction_operation() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: true,
        ..RuntimeConfig::default()
    })
    .unwrap();
    let content = serde_json::to_string(
        &(0..40)
            .map(|index| {
                serde_json::json!({
                    "id": index,
                    "message": format!("record-{index}-{}", "x".repeat(80))
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let attribution = ProtocolAttribution {
        agent_id: "agentscope".to_string(),
        session_id: Some("record-session".to_string()),
        tool_use_id: Some("record-call".to_string()),
    };
    let response = runtime
        .post_tool(
            &PostToolRequest {
                result_kind: ResultKind::Tool,
                tool_name: "api".to_string(),
                content,
                status: ToolResultStatus::Success,
                content_origin: ContentOrigin::ApiResponse,
                command: None,
                output_optimization: OutputOptimization::None,
                capabilities: PostToolCapabilities {
                    replace_output: true,
                    recovery: tokenless_protocol::RecoveryMethod::Shell,
                    replace_with_text: false,
                },
                exit_code: None,
                stderr: None,
                original_command: None,
                cwd: None,
            },
            &attribution,
        )
        .unwrap();
    assert_eq!(
        response.applied_operations,
        [tokenless_protocol::AppliedOperation::JsonRecordReduction]
    );

    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    let records = recorder.records_by_session("record-session", None).unwrap();
    assert_eq!(
        records[0].applied_operations,
        Some(vec!["json_record_reduction".to_string()])
    );
}

#[test]
fn runtime_records_build_log_reduction_operation() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: true,
        ..RuntimeConfig::default()
    })
    .unwrap();
    let mut content = "$ cargo build\n".to_owned();
    for index in 0..30 {
        content.push_str(&format!(
            "Compiling package-{index:03} v0.1.{index} with extended progress output\n"
        ));
    }
    content.push_str("Finished `dev` profile [unoptimized] target(s) in 1.2s\n");
    let response = runtime
        .post_tool(
            &PostToolRequest {
                result_kind: ResultKind::Tool,
                tool_name: "Bash".to_owned(),
                content,
                status: ToolResultStatus::Success,
                content_origin: ContentOrigin::CommandOutput,
                command: None,
                output_optimization: OutputOptimization::None,
                capabilities: PostToolCapabilities {
                    replace_output: true,
                    recovery: tokenless_protocol::RecoveryMethod::Shell,
                    replace_with_text: true,
                },
                exit_code: None,
                stderr: None,
                original_command: None,
                cwd: None,
            },
            &ProtocolAttribution {
                agent_id: "agentscope".to_owned(),
                session_id: Some("build-log-session".to_owned()),
                tool_use_id: Some("build-log-call".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        response.applied_operations,
        [tokenless_protocol::AppliedOperation::BuildLogReduction]
    );

    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    let records = recorder
        .records_by_session("build-log-session", None)
        .unwrap();
    assert_eq!(records[0].content_type.as_deref(), Some("build_log"));
    assert_eq!(
        records[0].applied_operations,
        Some(vec!["build_log_reduction".to_owned()])
    );
}

#[test]
fn concurrent_sessions_keep_embedded_html_rows_and_attribution_apart() {
    // Four hook processes sharing one state directory: each session's
    // document repeats one shared fragment and carries its own, so the
    // shared row is created by one session and refreshed by the others.
    let directory = tempfile::tempdir().unwrap();
    let fragment = |label: &str| {
        format!(
            "<div style=\"font-family: Arial, sans-serif; color: #333333\">\
             <p style=\"margin: 0 0 8px 0\"><span style=\"font-weight: normal\">{label}: \
             approve the refund before</span> <strong>2024-06-01</strong>.</p>\
             <ul style=\"padding-left: 16px; margin: 0\"><li style=\"margin: 0\">\
             <a href=\"https://example.com/{label}\" target=\"_blank\" \
             rel=\"noopener noreferrer\">Reference</a></li></ul></div>"
        )
    };
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for session in 0..4 {
            let (directory, barrier) = (directory.path(), &barrier);
            scope.spawn(move || {
                let runtime = TokenlessRuntime::new(RuntimeConfig {
                    data_dir: Some(directory.to_path_buf()),
                    stats_enabled: true,
                    ..RuntimeConfig::default()
                })
                .unwrap();
                let attribution = ProtocolAttribution {
                    agent_id: "qwenwork".to_owned(),
                    session_id: Some(format!("session-{session}")),
                    tool_use_id: Some(format!("call-{session}")),
                };
                let own = fragment(&format!("session-{session}"));
                let content = serde_json::to_string(&serde_json::json!({
                    "session": session,
                    "shared": fragment("shared"),
                    "own": own,
                }))
                .unwrap();
                barrier.wait();
                let response = runtime
                    .post_tool(
                        &PostToolRequest {
                            result_kind: ResultKind::Tool,
                            tool_name: "fetch_record".to_owned(),
                            content,
                            status: ToolResultStatus::Success,
                            content_origin: ContentOrigin::ApiResponse,
                            command: None,
                            output_optimization: OutputOptimization::None,
                            capabilities: PostToolCapabilities {
                                replace_output: true,
                                recovery: tokenless_protocol::RecoveryMethod::Shell,
                                replace_with_text: false,
                            },
                            exit_code: None,
                            stderr: None,
                            original_command: None,
                            cwd: None,
                        },
                        &attribution,
                    )
                    .unwrap();
                assert_eq!(
                    response.applied_operations,
                    [tokenless_protocol::AppliedOperation::HtmlExtraction]
                );
                let output: serde_json::Value = serde_json::from_str(&response.output).unwrap();
                assert_eq!(output["session"], session);
                for (field, expected) in [("shared", fragment("shared")), ("own", own)] {
                    let text = output[field].as_str().unwrap();
                    let hash = tokenless_ccr::recovery_hashes(
                        text,
                        &tokenless_protocol::RecoveryMethod::Shell,
                    )[0];
                    let restored = runtime
                        .retrieve_authorized(
                            &RetrieveRequest {
                                hash_or_marker: hash.to_owned(),
                                visible_markers: vec![text.to_owned()],
                            },
                            &attribution,
                        )
                        .unwrap();
                    assert_eq!(restored.payload, expected);
                }
            });
        }
    });

    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    for session in 0..4 {
        let records = recorder
            .records_by_session(&format!("session-{session}"), None)
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].tool_use_id.as_deref(),
            Some(format!("call-{session}").as_str())
        );
        assert_eq!(records[0].content_type.as_deref(), Some("json"));
        assert_eq!(
            records[0].applied_operations,
            Some(vec!["html_extraction".to_owned()])
        );
    }
    let connection = rusqlite::Connection::open(directory.path().join("stats.db")).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT session_id, tool_use_id, COUNT(*) FROM retrieve_events \
             WHERE outcome = 'hit' GROUP BY session_id, tool_use_id ORDER BY session_id",
        )
        .unwrap();
    let events = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        events,
        (0..4)
            .map(|session| (format!("session-{session}"), format!("call-{session}"), 2))
            .collect::<Vec<_>>()
    );
}

#[test]
fn schema_compression_is_reversible_and_attributed() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: true,
        ..RuntimeConfig::default()
    })
    .unwrap();
    let description = format!("SCHEMA_SENTINEL {}", "details ".repeat(200));
    let input = serde_json::to_string(&serde_json::json!({
        "type": "function",
        "function": {
            "name": "lookup",
            "description": description,
            "parameters": {"type": "object", "properties": {}}
        }
    }))
    .unwrap();
    let attribution = Attribution {
        agent_id: "python-sdk".to_string(),
        session_id: Some("schema-session".to_string()),
        tool_use_id: None,
    };
    let result = runtime.compress_schema(&input, &attribution).unwrap();
    assert!(result.applied());
    let hash = extract_hash(&result.output).unwrap();
    assert_eq!(runtime.retrieve(hash).unwrap(), description);

    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    let records = recorder.records_by_session("schema-session", None).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].operation, OperationType::CompressSchema);
}

#[test]
fn schema_compression_handles_tools_request_container() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        ..RuntimeConfig::default()
    })
    .unwrap();
    let input = serde_json::to_string(&serde_json::json!({
        "model": "example-model",
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup",
                "description": "A".repeat(2000),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "B".repeat(1000)
                        }
                    }
                }
            }
        }]
    }))
    .unwrap();

    let result = runtime
        .compress_schema(&input, &Attribution::new("runtime-test"))
        .unwrap();
    assert!(result.applied());

    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(output["model"], "example-model");
    assert!(
        output["tools"][0]["function"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 256
    );
    assert!(
        output["tools"][0]["function"]["parameters"]["properties"]["query"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 160
    );
}

#[test]
fn schema_no_savings_rolls_back_stash() {
    let input = r#"{"type":"function","function":{"name":"small","parameters":{}}}"#;
    let store = Arc::new(InMemoryStore::new()) as Arc<dyn StashStore>;
    let result = compress_schema_with_store(input, true, Some(&store)).unwrap();
    assert_eq!(result.disposition, Disposition::NoSavings);
    assert_eq!(result.output, input);
    assert_eq!(store.len(), 0);
}

/// State directory whose `stash.db` exists but is not a SQLite database.
///
/// A non-empty file with an invalid header satisfies the shared path
/// policy (regular file inside the state directory) yet cannot be opened
/// as a database, so construction continues with `stash_store = None`
/// while `stats.db` still opens normally. Unlike a read-only directory,
/// this also holds for a runtime running as root.
fn unopenable_stash_dir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("stash.db"), "not a sqlite database\n").unwrap();
    directory
}

/// BeforeModel request carrying a declaration long enough to compress.
fn long_tool_request() -> BeforeModelRequest {
    BeforeModelRequest {
        tools: vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "lookup",
                "description": format!("SCHEMA_SENTINEL {}", "details ".repeat(200)),
                "parameters": {"type": "object", "properties": {}}
            }
        })],
        visible_context: serde_json::json!({"messages": []}),
        capabilities: tokenless_protocol::BeforeModelCapabilities {
            replace_tools: true,
            recovery: tokenless_protocol::RecoveryMethod::tool("tokenless_retrieve").unwrap(),
        },
    }
}

/// Entry options matching the ones `TokenlessRuntime::before_model` builds
/// from a default [`RuntimeConfig`].
fn runtime_entry_options() -> EntryOptions {
    let config = RuntimeConfig::default();
    EntryOptions {
        compression_enabled: config.compression_enabled,
        search_path_sharing_enabled: config.search_path_sharing_enabled,
        diff_compression_enabled: config.diff_compression_enabled,
        html_extraction_enabled: config.html_extraction_enabled,
        browser_snapshot_enabled: config.browser_snapshot_enabled,
        mixed_content_enabled: config.mixed_content_enabled,
        stash_enabled: true,
        rtk_launcher: None,
        rtk_data_dir: None,
    }
}

#[test]
fn html_rendering_and_diff_cropping_are_on_by_default() {
    let config = RuntimeConfig::default();
    assert!(config.html_extraction_enabled);
    assert!(config.diff_compression_enabled);
    assert!(config.browser_snapshot_enabled);
}

#[test]
fn before_model_without_a_stash_store_keeps_tools_and_records_nothing() {
    let directory = unopenable_stash_dir();
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: true,
        ..RuntimeConfig::default()
    })
    .unwrap();
    assert!(!runtime.stash_available());
    assert!(runtime.stash_error().is_some());
    assert!(runtime.stats_available());

    let request = long_tool_request();
    let attribution = ProtocolAttribution {
        agent_id: "runtime-test".to_string(),
        session_id: Some("schema-no-stash".to_string()),
        tool_use_id: None,
    };
    let response = runtime.before_model(&request, &attribution).unwrap();

    // Fail-open: the model still sees the original declarations and no
    // marker advertises a stash that could not be opened.
    assert_eq!(response.tools, request.tools);
    assert!(response.visible_markers.is_empty());

    // `BeforeModelResponse` carries no disposition, so read the reason
    // from the entry router the runtime delegates to: it reports the
    // candidate as unrecoverable and keeps the measurement at the input,
    // which is what makes the two token estimates come out equal.
    let outcome = before_model_with_store(&request, &runtime_entry_options(), None).unwrap();
    assert_eq!(
        outcome.stats.disposition,
        Disposition::RecoverabilityUnavailable
    );
    assert_eq!(outcome.stats.measured_output, outcome.stats.input);
    assert!(outcome.artifact_keys.is_empty());

    // An unmeasured candidate writes no statistics record at all, even
    // though the statistics database opened successfully.
    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    assert_eq!(recorder.count().unwrap(), 0);
    assert!(
        recorder
            .records_by_session("schema-no-stash", None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn before_model_dry_run_without_a_stash_store_is_recorded() {
    let directory = unopenable_stash_dir();
    // `TOKENLESS_COMPRESSION_ENABLED=0` reaches the runtime as
    // `compression_enabled: false`.
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: true,
        compression_enabled: false,
        ..RuntimeConfig::default()
    })
    .unwrap();
    assert!(!runtime.stash_available());
    assert!(runtime.stats_available());

    let request = long_tool_request();
    let attribution = ProtocolAttribution {
        agent_id: "runtime-test".to_string(),
        session_id: Some("schema-no-stash-dry-run".to_string()),
        tool_use_id: None,
    };
    let response = runtime.before_model(&request, &attribution).unwrap();
    assert_eq!(response.tools, request.tools);

    // Dry run is measured, so the predicted savings are recorded even
    // without a stash store.
    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    let records = recorder
        .records_by_session("schema-no-stash-dry-run", None)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].operation, OperationType::CompressSchema);
    assert_eq!(records[0].mode, CompressionMode::DryRun);
    assert!(records[0].after_tokens < records[0].before_tokens);
}

#[test]
fn toon_uses_only_a_smaller_encoding() {
    let input = serde_json::to_string(&serde_json::json!({
        "items": (0..100)
            .map(|index| serde_json::json!({"name": "same", "value": index}))
            .collect::<Vec<_>>()
    }))
    .unwrap();
    let result = compress_toon(&input, true, MIN_TOON_CHARS).unwrap();
    assert!(result.applied());
    assert!(result.after_tokens < result.before_tokens);

    // A tiny payload never reaches the encoder now: the shared
    // minimum-length gate passes it through untouched.
    let tiny = "null";
    let result = compress_toon(tiny, true, MIN_TOON_CHARS).unwrap();
    assert_eq!(result.disposition, Disposition::Passthrough);
    assert_eq!(result.output, tiny);

    // The same tiny payload encodes when the gate is disabled.
    let result = compress_toon(tiny, true, 0).unwrap();
    assert_eq!(result.disposition, Disposition::NoSavings);
    assert_eq!(result.output, tiny);
}

#[test]
fn toon_skips_short_payloads_with_savings() {
    // Uniform rows compress well under TOON, so without the gate this
    // payload would be Applied; the minimum-length check must win.
    let input = serde_json::to_string(&serde_json::json!({
        "items": (0..10)
            .map(|index| serde_json::json!({"name": "same", "value": index}))
            .collect::<Vec<_>>()
    }))
    .unwrap();
    assert!(input.chars().count() < MIN_TOON_CHARS);
    let gated = compress_toon(&input, true, MIN_TOON_CHARS).unwrap();
    assert_eq!(gated.disposition, Disposition::Passthrough);
    assert_eq!(gated.output, input);
    assert_eq!(gated.before_tokens, gated.after_tokens);

    let forced = compress_toon(&input, true, 0).unwrap();
    assert!(forced.applied());
    assert!(forced.after_tokens < forced.before_tokens);
}

#[test]
fn toon_min_chars_boundary_is_inclusive() {
    // Build {"pad":"aaa..."} with exactly MIN_TOON_CHARS characters.
    let overhead = r#"{"pad":""}"#.chars().count();
    let padded = format!(r#"{{"pad":"{}"}}"#, "a".repeat(MIN_TOON_CHARS - overhead));
    assert_eq!(padded.chars().count(), MIN_TOON_CHARS);
    // At exactly the threshold the gate must not fire; the savings
    // comparison alone decides the disposition.
    let result = compress_toon(&padded, true, MIN_TOON_CHARS).unwrap();
    assert_ne!(result.disposition, Disposition::Passthrough);
}

#[test]
fn toon_gate_does_not_skip_json_validation() {
    // Short invalid input must fail with InvalidJson even under the
    // default gate: the gate skips TOON encoding, not JSON validation.
    let error = compress_toon("not json", true, MIN_TOON_CHARS).unwrap_err();
    assert!(matches!(error, RuntimeError::InvalidJson(_)));

    // The same contract holds with the gate disabled.
    let error = compress_toon("not json", true, 0).unwrap_err();
    assert!(matches!(error, RuntimeError::InvalidJson(_)));
}

/// 40 uniform records keyed by ID: the shape TOON spec v4.4 encodes as a
/// keyed tabular map, short enough to win the size comparison where 0.5.0
/// expanded every entry into an indented block longer than compact JSON.
fn keyed_records(ratio: &str) -> String {
    let entries = (0..40)
        .map(|index| {
            let ratio = if index == 3 { ratio } else { "0.5" };
            format!(r#""u{index}":{{"name":"user{index}","active":true,"ratio":{ratio}}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"users":{{{entries}}}}}"#)
}

#[test]
fn toon_declines_numbers_it_cannot_retain() {
    // The control proves this shape really does reach TOON, so every
    // rejection below is the numeric gate rather than a size accident.
    let clean = keyed_records("0.5");
    assert!(clean.chars().count() >= MIN_TOON_CHARS);
    let control = compress_toon(&clean, true, MIN_TOON_CHARS).unwrap();
    assert!(control.applied());
    assert!(control.after_tokens < control.before_tokens);

    // `1e-400` underflows, `1.234567890123456789` rounds and
    // `18446744073709551617` exceeds `u64`, so the literal scan rejects
    // all three. `-0.0` and `1e3` survive the scan and are caught only by
    // the decode round trip: TOON writes them as `0` and `1000`, losing
    // the sign and the float type.
    for ratio in [
        "1e-400",
        "-0.0",
        "1e3",
        "1.234567890123456789",
        "18446744073709551617",
    ] {
        let input = keyed_records(ratio);
        let result = compress_toon(&input, true, MIN_TOON_CHARS).unwrap();
        assert_eq!(
            result.disposition,
            Disposition::Passthrough,
            "ratio {ratio} must not be encoded"
        );
        assert_eq!(result.output, input, "ratio {ratio} must survive verbatim");
        assert_eq!(
            result.compressed_output, input,
            "ratio {ratio} must not report a candidate"
        );
        assert_eq!(result.before_tokens, result.after_tokens, "ratio {ratio}");
    }
}

#[test]
fn toon_numeric_gate_also_governs_dry_run() {
    // Dry run predicts what an enabled run would emit, so a payload the
    // gate declines must not be booked as a predicted saving either.
    let input = keyed_records("1e-400");
    let result = compress_toon(&input, false, MIN_TOON_CHARS).unwrap();
    assert_eq!(result.disposition, Disposition::Passthrough);
    assert_eq!(result.output, input);
    assert_eq!(result.compressed_output, input);
}

#[test]
fn toon_numeric_gate_ignores_the_length_override() {
    // `min_toon_chars = 0` opts out of the length gate only. A caller
    // asking for short payloads to be encoded still gets exact numbers.
    let input = r#"{"a":[1e-400,1e-400,1e-400]}"#;
    let result = compress_toon(input, true, 0).unwrap();
    assert_eq!(result.disposition, Disposition::Passthrough);
    assert_eq!(result.output, input);
}

#[test]
fn invalid_explicit_data_dir_is_rejected() {
    let error = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(PathBuf::from("relative")),
        ..RuntimeConfig::default()
    })
    .err()
    .expect("relative state directory must fail");
    assert!(matches!(error, RuntimeError::InvalidStatePath(_)));
}

#[cfg(unix)]
#[test]
fn sqlite_open_failure_remains_fail_open() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    // Directory mode bits do not constrain euid 0; skip rather than
    // asserting a permission failure that cannot happen as root.
    if std::fs::File::create(directory.path().join(".probe")).is_ok() {
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let runtime = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(directory.path().to_path_buf()),
        stats_enabled: false,
        ..RuntimeConfig::default()
    })
    .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!runtime.stash_available());
    assert!(runtime.stash_error().is_some());

    let input = long_response();
    let result = runtime
        .compress_response(
            &input,
            &CompressOptions {
                truncate_arrays_at: Some(2),
                require_reversible: true,
                ..CompressOptions::default()
            },
            &Attribution::new("test"),
        )
        .unwrap();
    assert_eq!(result.output, input);
}

#[test]
fn retrieve_recorded_logs_each_store_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let recorder = StatsRecorder::new(directory.path().join("stats.db")).unwrap();
    let store = InMemoryStore::new();
    let write = store.stash("recorded payload").unwrap();

    let payload = retrieve_recorded(&store, &write.key, Some(&recorder), "cli").unwrap();
    assert_eq!(payload, "recorded payload");
    let miss = retrieve_recorded(&store, &"0".repeat(24), Some(&recorder), "mcp");
    assert!(matches!(miss, Err(RuntimeError::StashEntryNotFound { .. })));

    struct FailingRetrieve;
    impl StashStore for FailingRetrieve {
        fn stash(&self, _payload: &str) -> Result<tokenless_ccr::StashWrite, StashError> {
            unreachable!()
        }
        fn retrieve(&self, _hash: &str) -> Result<Option<String>, StashError> {
            Err(StashError::Backend("simulated".to_string()))
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
    let error = retrieve_recorded(&FailingRetrieve, &write.key, Some(&recorder), "embedded");
    assert!(matches!(error, Err(RuntimeError::StashRetrieve(_))));

    // Invalid input never reaches the store: no event.
    let invalid = retrieve_recorded(&store, "not-a-hash", Some(&recorder), "cli");
    assert!(matches!(invalid, Err(RuntimeError::InvalidHash { .. })));

    let totals = recorder.retrieve_totals().unwrap();
    assert_eq!(totals.hits, 1);
    assert_eq!(totals.misses, 1);
    assert_eq!(totals.errors, 1);
    assert!(totals.retrieved_tokens > 0);
}
