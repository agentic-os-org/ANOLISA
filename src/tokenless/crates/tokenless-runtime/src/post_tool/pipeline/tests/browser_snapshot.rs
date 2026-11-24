use super::*;

fn pw_snapshot_input(links: usize) -> String {
    let mut tree = String::from("- navigation [ref=e1]:\n");
    for i in 0..links {
        tree.push_str(&format!(
            "  - link \"Story number {i} with a reasonably long title\" [ref=e{}] [cursor=pointer]:\n    - /url: https://news.example.com/item?id={i}\n",
            i + 2
        ));
    }
    format!(
        "### Page\n- Page URL: https://news.example.com/\n- Page Title: News\n### Snapshot\n```yaml\n{tree}\n```"
    )
}

fn cdt_snapshot_input(links: usize) -> String {
    // A clean name-only listing: even this unambiguous-looking shape passes
    // through, because the raw-quote format cannot be decomposed losslessly.
    let mut body = String::from("uid=1_0 RootWebArea \"News\" busy\n");
    for i in 0..links {
        body.push_str(&format!(
            "  uid=1_{} link \"Story number {i} with a reasonably long title\"\n",
            i + 1
        ));
    }
    format!("## Latest page snapshot\n{body}")
}

#[test]
fn browser_snapshot_is_switchable_and_requires_recovery_and_text_replacement() {
    let input = pw_snapshot_input(40);
    for mode in 0..11 {
        let mut req = request(&input);
        let mut config = build_log_config();
        config.browser_snapshot_enabled = true;
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        match mode {
            0 => config.browser_snapshot_enabled = false,
            1 => config.stash_enabled = false,
            2 => req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None,
            3 => req.capabilities.replace_output = false,
            4 => req.capabilities.replace_with_text = false,
            5 => req.content_origin = ContentOrigin::FileContent,
            6 => req.tool_name = "Grep".into(),
            7 => req.status = ToolResultStatus::Error,
            8 => config.max_input_bytes = input.len() - 1,
            9 => req.content_origin = ContentOrigin::FileRead,
            10 => {}
            _ => unreachable!(),
        }
        let run =
            PostToolPipeline::run(&req, &config, if mode == 10 { None } else { Some(&store) });
        assert_eq!(run.response.output, input, "mode {mode}");
        assert!(run.operations.is_empty(), "mode {mode}");
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn browser_snapshot_kill_switch_emits_no_new_wire_values() {
    // Strict protocol parsers have no fallback variant: with the domain
    // disabled, even the passthrough label must stay on an existing wire
    // value so old consumers never see browser_snapshot.
    for input in [pw_snapshot_input(40), cdt_snapshot_input(40)] {
        let mut config = build_log_config();
        config.browser_snapshot_enabled = false;
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
        assert_eq!(run.response.disposition, Disposition::Passthrough);
        assert_eq!(run.response.content_type, Some(ContentType::PlainText));
        assert!(run.response.applied_operations.is_empty());
        assert_eq!(run.response.output, input);
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn browser_snapshot_compresses_from_commands_and_apis_with_one_stash_write() {
    let input = pw_snapshot_input(40);
    for origin in [ContentOrigin::CommandOutput, ContentOrigin::ApiResponse] {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut config = build_log_config();
        config.browser_snapshot_enabled = true;
        let mut req = request(&input);
        req.content_origin = origin;
        let run = PostToolPipeline::run(&req, &config, Some(&store));

        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(
            run.response.content_type,
            Some(ContentType::BrowserSnapshot)
        );
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::BrowserSnapshotReduction]
        );
        assert_eq!(run.response.recoverability, Recoverability::Retrievable);
        let output = &run.response.output;
        assert!(
            output.starts_with("If needed, run in shell: tokenless retrieve "),
            "{output}"
        );
        assert!(output.contains("depth\trole\tname\tref\tattrs\turl\ttext"));
        assert!(output.contains("### Page\n"));
        assert!(run.response.before_tokens - run.response.after_tokens >= 16);
        assert_eq!(run.response.stash_keys.len(), 1);
        assert_eq!(
            concrete.retrieve(&run.response.stash_keys[0]).unwrap(),
            Some(input.clone())
        );
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
        assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn browser_snapshot_cdt_passes_through_unchanged() {
    // chrome-devtools uid snapshots are recognized but never compressed: their
    // raw-quote serialization has no explicit record boundaries, so the domain
    // fails open, keeps the original bytes, and writes no stash entry.
    let input = cdt_snapshot_input(120);
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let mut config = build_log_config();
    config.browser_snapshot_enabled = true;
    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(
        run.response.content_type,
        Some(ContentType::BrowserSnapshot)
    );
    assert_eq!(run.response.output, input);
    assert!(run.response.applied_operations.is_empty());
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn browser_snapshot_failures_pass_through_and_rejections_roll_back() {
    for mode in 0..4 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut config = build_log_config();
        config.browser_snapshot_enabled = true;
        let (input, disposition, writes, deletes) = match mode {
            // A one-link snapshot compresses marginally: the stash write is
            // rolled back when arbitration rejects savings below 16 tokens.
            0 => (pw_snapshot_input(1), Disposition::NoSavings, 1, 1),
            // Detected shape, malformed tree: the compressor fails open.
            1 => (
                "### Page\n- Page URL: https://x/\n### Snapshot\n```yaml\n- generic [ref=e1]:\n   - link \"x\" [ref=e2]\n```".to_owned(),
                Disposition::NoSavings,
                0,
                0,
            ),
            2 => {
                config.timeout = Duration::ZERO;
                (pw_snapshot_input(40), Disposition::Timeout, 1, 1)
            }
            3 => {
                config.compression_enabled = false;
                (pw_snapshot_input(40), Disposition::DryRun, 0, 0)
            }
            _ => unreachable!(),
        };
        let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
        assert_eq!(run.response.disposition, disposition, "mode {mode}");
        assert_eq!(run.response.output, input);
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(
            concrete.stash_calls.load(Ordering::Relaxed),
            writes,
            "mode {mode}"
        );
        assert_eq!(
            concrete.delete_calls.load(Ordering::Relaxed),
            deletes,
            "mode {mode}"
        );
        assert_eq!(concrete.len(), 0);
        if mode == 3 {
            assert_eq!(run.operations, [AppliedOperation::BrowserSnapshotReduction]);
        }
    }
}

#[test]
fn browser_snapshot_stash_failure_degrades_to_unchanged_and_counts_one_error() {
    let input = pw_snapshot_input(40);
    let mut config = build_log_config();
    config.browser_snapshot_enabled = true;
    let store: Arc<dyn StashStore> = Arc::new(CountingStore {
        stash_calls: std::sync::atomic::AtomicUsize::new(1),
        fail_stashes_after: 1,
        ..CountingStore::default()
    });
    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
    assert_eq!(run.response.output, input);
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert!(run.response.applied_operations.is_empty());
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(run.stash_errors, Some(1));
    assert_eq!(run.stash_writes, Some(0));
}

#[test]
fn diffs_of_snapshot_files_keep_the_diff_domain() {
    let input = "diff --git a/snap.yml b/snap.yml\n--- a/snap.yml\n+++ b/snap.yml\n@@ -1,3 +1,3 @@\n - link \"x\" [ref=e1]\n+- link \"y\" [ref=e2]\n - link \"z\" [ref=e3]\n".to_owned();
    let mut config = build_log_config();
    config.browser_snapshot_enabled = true;
    config.diff_compression_enabled = true;
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
    assert_eq!(run.response.content_type, Some(ContentType::Diff));
}

#[test]
fn browser_find_compresses_with_stash_recovery_and_respects_kill_switch() {
    let mut blocks = Vec::with_capacity(30);
    for i in 0..30 {
        blocks.push(format!(
            "- link \"Story number {i} with a reasonably long title\" [ref=e{}] [cursor=pointer]:\n  - /url: https://news.example.com/item?id={i}",
            i + 10
        ));
    }
    let input = format!("### Result\n{}", blocks.join("\n----\n"));

    let mut disabled = build_log_config();
    disabled.browser_snapshot_enabled = false;
    let store_off = Arc::new(CountingStore::default());
    let store_off_dyn: Arc<dyn StashStore> = store_off.clone();
    let run_off = PostToolPipeline::run(&request(&input), &disabled, Some(&store_off_dyn));
    assert_eq!(run_off.response.disposition, Disposition::Passthrough);
    assert_eq!(run_off.response.content_type, Some(ContentType::PlainText));
    assert_eq!(run_off.response.output, input);
    assert_eq!(store_off.stash_calls.load(Ordering::Relaxed), 0);

    let mut enabled = build_log_config();
    enabled.browser_snapshot_enabled = true;
    let store_on = Arc::new(CountingStore::default());
    let store_on_dyn: Arc<dyn StashStore> = store_on.clone();
    let run_on = PostToolPipeline::run(&request(&input), &enabled, Some(&store_on_dyn));
    assert_eq!(run_on.response.disposition, Disposition::Applied);
    assert_eq!(
        run_on.response.content_type,
        Some(ContentType::BrowserSnapshot)
    );
    assert_eq!(
        run_on.response.applied_operations,
        [AppliedOperation::BrowserSnapshotReduction]
    );
    assert_eq!(run_on.response.recoverability, Recoverability::Retrievable);
    assert!(
        run_on
            .response
            .output
            .contains("block\tdepth\trole\tname\tref\tattrs\turl\ttext")
    );
    assert_eq!(
        store_on.retrieve(&run_on.response.stash_keys[0]).unwrap(),
        Some(input)
    );
}

#[test]
fn browser_find_with_upstream_summary_compresses_and_recovers_byte_exact() {
    let mut blocks = Vec::with_capacity(30);
    for i in 0..30 {
        blocks.push(format!(
            "- link \"Story number {i} with a reasonably long title\" [ref=e{}] [cursor=pointer]:\n  - /url: https://news.example.com/item?id={i}",
            i + 10
        ));
    }
    let input = format!(
        "### Result\nFound 30 matches for \"Story\":\n\n{}\n",
        blocks.join("\n----\n")
    );

    let mut disabled = build_log_config();
    disabled.browser_snapshot_enabled = false;
    let mut req_off = request(&input);
    req_off.content_origin = ContentOrigin::ApiResponse;
    let store_off = Arc::new(CountingStore::default());
    let store_off_dyn: Arc<dyn StashStore> = store_off.clone();
    let run_off = PostToolPipeline::run(&req_off, &disabled, Some(&store_off_dyn));
    assert_eq!(run_off.response.disposition, Disposition::Passthrough);
    assert_eq!(run_off.response.content_type, Some(ContentType::PlainText));
    assert_eq!(run_off.response.output, input);
    assert_eq!(store_off.stash_calls.load(Ordering::Relaxed), 0);

    let mut enabled = build_log_config();
    enabled.browser_snapshot_enabled = true;
    let mut req_on = request(&input);
    req_on.content_origin = ContentOrigin::ApiResponse;
    let store_on = Arc::new(CountingStore::default());
    let store_on_dyn: Arc<dyn StashStore> = store_on.clone();
    let run_on = PostToolPipeline::run(&req_on, &enabled, Some(&store_on_dyn));
    assert_eq!(run_on.response.disposition, Disposition::Applied);
    assert_eq!(
        run_on.response.content_type,
        Some(ContentType::BrowserSnapshot)
    );
    assert_eq!(
        run_on.response.applied_operations,
        [AppliedOperation::BrowserSnapshotReduction]
    );
    assert!(
        run_on
            .response
            .output
            .contains("Found 30 matches for \"Story\":")
    );
    assert!(
        run_on
            .response
            .output
            .contains("block\tdepth\trole\tname\tref\tattrs\turl\ttext")
    );
    assert!(run_on.response.output.contains("\te10\t"));
    assert_eq!(
        store_on.retrieve(&run_on.response.stash_keys[0]).unwrap(),
        Some(input)
    );
}

#[test]
fn browser_find_folded_name_containing_heading_compresses_through_post_tool() {
    let mut blocks = Vec::with_capacity(30);
    blocks.push(
        "- link \"Story number 0\n  ### Events\n  with a long folded title\" [ref=e10] [cursor=pointer]:\n  - /url: https://news.example.com/item?id=0".to_owned()
    );
    for i in 1..30 {
        blocks.push(format!(
            "- link \"Story number {i} with a reasonably long title\" [ref=e{}] [cursor=pointer]:\n  - /url: https://news.example.com/item?id={i}",
            i + 10
        ));
    }
    let input = format!(
        "### Result\nFound 30 matches for \"Story\":\n\n{}\n### Events\n- ready",
        blocks.join("\n----\n")
    );

    let mut config = build_log_config();
    config.browser_snapshot_enabled = true;
    let mut req = request(&input);
    req.content_origin = ContentOrigin::ApiResponse;
    let store = Arc::new(CountingStore::default());
    let store_dyn: Arc<dyn StashStore> = store.clone();
    let run = PostToolPipeline::run(&req, &config, Some(&store_dyn));
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(
        run.response.content_type,
        Some(ContentType::BrowserSnapshot)
    );
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::BrowserSnapshotReduction]
    );
    assert!(run.response.output.contains("\te10\t"));
    assert_eq!(
        store.retrieve(&run.response.stash_keys[0]).unwrap(),
        Some(input)
    );
}

#[test]
fn browser_find_with_context_gaps_compresses_and_recovers_byte_exact() {
    let mut blocks = Vec::with_capacity(30);
    for i in 0..30 {
        blocks.push(format!(
            "- toolbar:\n  ...\n  - link \"Story number {i} with a reasonably long title\" [ref=e{}] [cursor=pointer]:\n    - /url: https://news.example.com/item?id={i}",
            i + 10
        ));
    }
    let input = format!(
        "### Result\nFound 30 matches for \"Story\":\n\n{}\n",
        blocks.join("\n----\n")
    );

    let mut disabled = build_log_config();
    disabled.browser_snapshot_enabled = false;
    let mut req_off = request(&input);
    req_off.content_origin = ContentOrigin::ApiResponse;
    let store_off = Arc::new(CountingStore::default());
    let store_off_dyn: Arc<dyn StashStore> = store_off.clone();
    let run_off = PostToolPipeline::run(&req_off, &disabled, Some(&store_off_dyn));
    assert_eq!(run_off.response.disposition, Disposition::Passthrough);
    assert_eq!(run_off.response.content_type, Some(ContentType::PlainText));
    assert_eq!(run_off.response.output, input);
    assert_eq!(store_off.stash_calls.load(Ordering::Relaxed), 0);

    let mut enabled = build_log_config();
    enabled.browser_snapshot_enabled = true;
    let mut req_on = request(&input);
    req_on.content_origin = ContentOrigin::ApiResponse;
    let store_on = Arc::new(CountingStore::default());
    let store_on_dyn: Arc<dyn StashStore> = store_on.clone();
    let run_on = PostToolPipeline::run(&req_on, &enabled, Some(&store_on_dyn));
    assert_eq!(run_on.response.disposition, Disposition::Applied);
    assert_eq!(
        run_on.response.content_type,
        Some(ContentType::BrowserSnapshot)
    );
    assert_eq!(
        run_on.response.applied_operations,
        [AppliedOperation::BrowserSnapshotReduction]
    );
    assert!(run_on.response.output.contains("\t1\t...\t\t\t\t\t\n"));
    assert!(run_on.response.output.contains("\te10\t"));
    assert_eq!(
        store_on.retrieve(&run_on.response.stash_keys[0]).unwrap(),
        Some(input)
    );
}

#[test]
fn browser_find_literal_ref_in_name_detects_and_compresses_real_refs() {
    let mut blocks = Vec::with_capacity(30);
    for i in 0..30 {
        blocks.push(format!(
            "- link \"Story [ref=e999] item {i} with long title\" [ref=e{}] [cursor=pointer]",
            i + 10
        ));
    }
    let input = format!(
        "### Result\nFound 30 matches for \"ref\":\n\n{}\n",
        blocks.join("\n----\n")
    );

    let mut config = build_log_config();
    config.browser_snapshot_enabled = true;
    let mut req = request(&input);
    req.content_origin = ContentOrigin::ApiResponse;
    let store = Arc::new(CountingStore::default());
    let store_dyn: Arc<dyn StashStore> = store.clone();
    let run = PostToolPipeline::run(&req, &config, Some(&store_dyn));
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(
        run.response.content_type,
        Some(ContentType::BrowserSnapshot)
    );
    assert!(run.response.output.contains("Story [ref=e999] item 0"));
    assert!(run.response.output.contains("\te10\t"));
    assert_eq!(
        store.retrieve(&run.response.stash_keys[0]).unwrap(),
        Some(input)
    );
}

#[test]
fn browser_find_orphan_url_under_context_gap_passes_through_without_stash() {
    let fixture = include_str!(
        "../../../../../tokenless-compressors/tests/fixtures/content/browser_find_multi_block.txt"
    );
    let mutated = fixture.replacen(
        "  ...\n",
        "  ...\n    - /url: https://example.test/orphan\n",
        1,
    );
    assert_ne!(mutated, fixture, "fixture must contain a `  ...` gap line");

    let mut config = build_log_config();
    config.browser_snapshot_enabled = true;
    let mut req = request(&mutated);
    req.content_origin = ContentOrigin::ApiResponse;
    let store = Arc::new(CountingStore::default());
    let store_dyn: Arc<dyn StashStore> = store.clone();
    let run = PostToolPipeline::run(&req, &config, Some(&store_dyn));
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(run.response.output, mutated);
    assert!(run.response.applied_operations.is_empty());
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(store.stash_calls.load(Ordering::Relaxed), 0);
}
