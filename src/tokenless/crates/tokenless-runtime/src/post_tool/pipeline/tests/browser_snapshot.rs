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
fn html_document_quoting_playwright_snapshot_stays_html() {
    let input = format!(
        "<!DOCTYPE html><html><head><title>Demo</title></head><body><pre>\n{}\n</pre></body></html>",
        pw_snapshot_input(25)
    );
    let mut config = build_log_config();
    config.browser_snapshot_enabled = true;
    config.html_extraction_enabled = true;
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
    assert_eq!(run.response.content_type, Some(ContentType::Html));
    assert!(
        !run.response
            .applied_operations
            .contains(&AppliedOperation::BrowserSnapshotReduction)
    );
    assert!(run.response.stash_keys.is_empty());
}
