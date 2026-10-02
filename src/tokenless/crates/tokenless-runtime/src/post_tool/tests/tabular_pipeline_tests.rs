fn tabular_input(delimiter: char) -> String {
    format!(
        "id{delimiter}message\r\n{}",
        (0..100)
            .map(|i| format!("{i}{delimiter}record-{i}-{}\r\n", "payload ".repeat(12)))
            .collect::<String>()
    )
}

#[test]
fn tabular_pipeline_commits_original_text_once() {
    for delimiter in [',', '\t'] {
        let input = tabular_input(delimiter);
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let run =
            PostToolPipeline::run(&request(&input), &build_log_config(), Some(&store)).unwrap();
        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.content_type, Some(ContentType::Tabular));
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::TabularRowReduction]
        );
        assert_eq!(run.response.recoverability, Recoverability::Retrievable);
        assert_eq!(run.response.stash_keys.len(), 1);
        assert_eq!(
            store
                .retrieve(&run.response.stash_keys[0])
                .unwrap()
                .as_deref(),
            Some(input.as_str())
        );
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
        assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn rectangular_source_and_prose_never_lose_lines() {
    for origin in [ContentOrigin::CommandOutput, ContentOrigin::ApiResponse] {
        for template in [
            "def function_{index}(a, b): return a + b",
            "assert_equal(expected_value_number_{index}, actual_value_number_{index})",
            "- Note {index}, the parser rejected the header row while processing request {index} in the ingestion pipeline",
        ] {
            let input = (0..100)
                .map(|i| template.replace("{index}", &i.to_string()))
                .collect::<Vec<_>>()
                .join("\n");
            let concrete = Arc::new(CountingStore::default());
            let store: Arc<dyn StashStore> = concrete.clone();
            let mut req = request(&input);
            req.content_origin = origin;
            let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store)).unwrap();
            assert_eq!(run.response.output, input);
            assert!(run.response.applied_operations.is_empty());
            assert!(run.response.stash_keys.is_empty());
            assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
        }
    }
}

#[test]
fn tabular_dry_run_and_timeout_do_not_leave_originals() {
    for dry_run in [true, false] {
        let input = tabular_input(',');
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut config = build_log_config();
        if dry_run {
            config.compression_enabled = false;
        } else {
            config.timeout = Duration::ZERO;
        }
        let run = PostToolPipeline::run(&request(&input), &config, Some(&store)).unwrap();
        assert_eq!(run.response.output, input);
        assert_eq!(
            run.response.disposition,
            if dry_run {
                Disposition::DryRun
            } else {
                Disposition::Timeout
            }
        );
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert!(concrete.is_empty());
        assert_eq!(
            concrete.stash_calls.load(Ordering::Relaxed),
            usize::from(!dry_run)
        );
        assert_eq!(
            concrete.delete_calls.load(Ordering::Relaxed),
            usize::from(!dry_run)
        );
        if dry_run {
            assert_eq!(run.operations, [AppliedOperation::TabularRowReduction]);
            assert!(run.response.after_tokens < run.response.before_tokens);
        }
    }
}

#[test]
fn tabular_respects_origin_status_and_text_capability() {
    let input = tabular_input(',');
    for mode in 0..4 {
        let mut req = request(&input);
        match mode {
            0 => req.content_origin = ContentOrigin::FileContent,
            1 => req.status = ToolResultStatus::Error,
            2 => req.capabilities.replace_output = false,
            3 => req.capabilities.replace_with_text = false,
            _ => unreachable!(),
        }
        let run = PostToolPipeline::run(&req, &build_log_config(), None).unwrap();
        assert_eq!(run.response.output, input);
        assert_eq!(run.response.disposition, Disposition::Passthrough);
        assert!(run.operations.is_empty());
    }
}

#[test]
fn tabular_full_candidate_needs_no_recovery_and_small_input_has_no_savings() {
    let input = "\"a\",\"b\"\r\n\"x\",\"y\"\r\n\"z\",\"w\"\r\n";
    let mut req = request(input);
    req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
    let run = PostToolPipeline::run(&req, &build_log_config(), None).unwrap();
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::TabularCompaction]
    );
    assert_eq!(run.response.recoverability, Recoverability::Lossless);
    assert!(run.response.stash_keys.is_empty());
    req.content = "a,b\nx,y\nz,w".into();
    let run = PostToolPipeline::run(&req, &build_log_config(), None).unwrap();
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(run.response.output, req.content);
}
