use std::sync::atomic::{AtomicUsize, Ordering};

use tokenless_ccr::StashError;
use tokenless_protocol::{OutputOptimization, PostToolCapabilities, ResultKind};

use super::*;

mod browser_snapshot;
mod embedded_html;
mod html;
mod mixed_content;
mod path_list;
mod tabular;

use path_list::path_list_input;
use tabular::tabular_input;

fn search_input() -> String {
    (1..30)
        .map(|line| format!("crates/long_directory/src/search_file.rs:{line}:  value  \r\n"))
        .collect()
}

#[test]
fn search_path_sharing_is_lossless_without_stash_or_recovery() {
    let input = search_input();
    let mut req = request(&input);
    req.content_origin = ContentOrigin::ApiResponse;
    req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.response.content_type, Some(ContentType::SearchResults));
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::SearchPathSharing]
    );
    assert_eq!(run.response.recoverability, Recoverability::Lossless);
    assert!(run.response.output.contains("29:  value  \r\n"));
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn grep_protection_is_independent_of_the_search_switch() {
    for (input, origin, content_type) in [
        (
            tabular_input(','),
            ContentOrigin::ApiResponse,
            ContentType::Tabular,
        ),
        (
            tabular_input('\t'),
            ContentOrigin::ApiResponse,
            ContentType::Tabular,
        ),
        (
            record_array(100),
            ContentOrigin::ApiResponse,
            ContentType::Json,
        ),
        (
            build_log(),
            ContentOrigin::CommandOutput,
            ContentType::BuildLog,
        ),
    ] {
        let mut req = request(&input);
        req.tool_name = "Grep".into();
        req.content_origin = origin;
        for enabled in [false, true] {
            let mut config = build_log_config();
            config.search_path_sharing_enabled = enabled;
            for force_json in [false, true] {
                let concrete = Arc::new(CountingStore::default());
                let store: Arc<dyn StashStore> = concrete.clone();
                config.force_json = force_json;
                let run = PostToolPipeline::run(&req, &config, Some(&store));
                assert_eq!(run.response.content_type, Some(content_type));
                assert_eq!(run.response.output, input);
                assert_eq!(
                    run.response.disposition,
                    if enabled {
                        Disposition::NoSavings
                    } else {
                        Disposition::Passthrough
                    }
                );
                assert!(run.operations.is_empty());
                assert!(run.response.applied_operations.is_empty());
                assert!(run.response.stash_keys.is_empty());
                assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
            }
            let mut other_tool = req.clone();
            other_tool.tool_name = "SearchFiles".into();
            config.force_json = false;
            let store: Arc<dyn StashStore> = Arc::new(CountingStore::default());
            let run = PostToolPipeline::run(&other_tool, &config, Some(&store));
            assert_eq!(run.response.disposition, Disposition::Applied);
            assert!(!run.operations.is_empty());
        }
    }
}

#[test]
fn shell_search_uses_only_the_search_compressor_for_every_format() {
    for command in [
        "grep -n match file",
        "cd /testbed && env LANG=C timeout 30 rg match src | sort",
    ] {
        for input in [
            search_input(),
            path_list_input(),
            tabular_input(','),
            record_array(100),
            build_log(),
            "41\n".into(),
            String::new(),
            "1:single-file match\n".repeat(30),
        ] {
            let mut req = request(&input);
            req.tool_name = "Bash".into();
            req.command = Some(command.into());
            req.content_origin = ContentOrigin::CommandOutput;
            for enabled in [false, true] {
                for force_json in [false, true] {
                    let mut config = build_log_config();
                    config.search_path_sharing_enabled = enabled;
                    config.force_json = force_json;
                    let concrete = Arc::new(CountingStore::default());
                    let store: Arc<dyn StashStore> = concrete.clone();
                    let run = PostToolPipeline::run(&req, &config, Some(&store));
                    let candidate = enabled
                        .then(|| SearchResultsCompressor.compress(&input))
                        .flatten();
                    assert_eq!(
                        run.response.output,
                        candidate.clone().unwrap_or_else(|| input.clone()),
                        "{command}"
                    );
                    assert_eq!(
                        run.response.disposition,
                        if candidate.is_some() {
                            Disposition::Applied
                        } else if enabled {
                            Disposition::NoSavings
                        } else {
                            Disposition::Passthrough
                        }
                    );
                    assert!(
                        run.response
                            .applied_operations
                            .iter()
                            .all(|op| *op == AppliedOperation::SearchPathSharing)
                    );
                    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
                }
            }
        }
    }
}

#[test]
fn native_grep_path_sharing_takes_precedence_over_force_json() {
    let mut req = request(&search_input());
    req.tool_name = "Grep".into();
    req.content_origin = ContentOrigin::ApiResponse;
    let run = PostToolPipeline::run(&req, &config(Duration::from_secs(1), 32), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::SearchPathSharing]
    );
    assert_eq!(run.response.recoverability, Recoverability::Lossless);
}

#[test]
fn compound_search_preserves_output_without_segment_boundaries() {
    for (command, input) in [
        (
            "grep -n foo src/a.py && cargo build",
            format!("1:foo\n{}", build_log()),
        ),
        (
            "grep -c '' *.log; cargo test",
            format!("build.log:41\n{}", build_log()),
        ),
        ("rg -l TODO src | xargs cat", record_array(100)),
        (
            "cargo build 2>&1 | grep -E 'error|warning'",
            "warning: diagnostic from the build\n".repeat(40),
        ),
        ("grep -q needle Cargo.toml && cargo build 2>&1", build_log()),
        ("rg --quiet needle Cargo.toml; cargo test", build_log()),
    ] {
        let mut req = request(&input);
        req.command = Some(command.into());
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
        assert_eq!(run.response.output, input, "{command}");
        assert_eq!(run.response.disposition, Disposition::NoSavings);
        assert!(run.response.applied_operations.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn command_search_protection_requires_command_output() {
    let input = record_array(100);
    for origin in [ContentOrigin::ApiResponse, ContentOrigin::FileRead] {
        let mut req = request(&input);
        req.tool_name = "Custom".into();
        req.content_origin = origin;
        let store: Arc<dyn StashStore> = Arc::new(CountingStore::default());
        let expected = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
        assert_eq!(expected.response.disposition, Disposition::Applied);
        req.command = Some("grep match file".into());
        let actual = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
        assert_eq!(actual.response.output, expected.response.output);
        assert_eq!(actual.response.disposition, expected.response.disposition);
    }
}

#[test]
fn search_path_sharing_respects_lifecycle_gates() {
    let input = search_input();
    for mode in 0..7 {
        let mut req = request(&input);
        req.content_origin = ContentOrigin::ApiResponse;
        let mut config = build_log_config();
        match mode {
            0 => req.content_origin = ContentOrigin::FileContent,
            1 => {
                req.status = ToolResultStatus::Error;
                req.exit_code = Some(127);
            }
            2 => req.capabilities.replace_output = false,
            3 => req.capabilities.replace_with_text = false,
            4 => config.max_input_bytes = input.len() - 1,
            5 => config.min_input_chars = input.chars().count() + 1,
            6 => config.timeout = Duration::ZERO,
            _ => unreachable!(),
        }
        let run = PostToolPipeline::run(&req, &config, None);
        assert_eq!(run.response.output, input);
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
    }
}

#[test]
fn search_dry_run_measures_candidate_without_replacing_output() {
    let input = search_input();
    let mut config = build_log_config();
    config.compression_enabled = false;
    let mut req = request(&input);
    req.content_origin = ContentOrigin::ApiResponse;
    let run = PostToolPipeline::run(&req, &config, None);
    assert_eq!(run.response.disposition, Disposition::DryRun);
    assert_eq!(run.response.output, input);
    assert!(run.response.applied_operations.is_empty());
    assert_eq!(run.operations, [AppliedOperation::SearchPathSharing]);
    assert!(run.response.after_tokens < run.response.before_tokens);
    assert!(run.response.stash_keys.is_empty());
}

#[test]
fn search_rejects_no_savings_and_unparsed_suffixes() {
    for input in [
        "x.rs:1:a\nx.rs:2:b\nx.rs:3:c".to_owned(),
        format!("{}--\n", search_input()),
    ] {
        for (tool, origin, command) in [
            ("Custom", ContentOrigin::ApiResponse, None),
            ("Grep", ContentOrigin::ApiResponse, None),
            (
                "Bash",
                ContentOrigin::CommandOutput,
                Some("grep -n match file"),
            ),
        ] {
            let mut req = request(&input);
            req.tool_name = tool.into();
            req.content_origin = origin;
            req.command = command.map(str::to_owned);
            let store: Arc<dyn StashStore> = Arc::new(CountingStore::default());
            let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
            assert_eq!(run.response.output, input);
            assert_eq!(run.response.disposition, Disposition::NoSavings);
            assert!(run.response.applied_operations.is_empty());
            assert_eq!(run.stash_writes, Some(0));
            assert_eq!(run.stash_errors, Some(0));
            assert_eq!(run.stash_size, Some(0));
        }
    }
}

#[test]
fn bracket_sniffed_non_json_passes_through_instead_of_failing() {
    // Two concatenated JSON documents look like JSON to the bracket
    // sniff but do not parse. The detector delegates authority to the
    // JSON compressor, so this misroute must degrade to passthrough
    // rather than failing the whole PostTool call.
    let input = "{\"items\": [1, 2, 3]}\n{\"second\": \"document\"}\n";
    let mut req = request(input);
    req.tool_name = "WebFetch".into();
    let run = PostToolPipeline::run(&req, &build_log_config(), None);
    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.output, input);
    assert_eq!(run.response.content_type, Some(ContentType::Json));
    assert!(run.response.applied_operations.is_empty());
    assert!(run.response.stash_keys.is_empty());
}

#[derive(Default)]
struct CountingStore {
    inner: InMemoryStore,
    stash_calls: AtomicUsize,
    delete_calls: AtomicUsize,
    /// When nonzero, `stash` starts failing after that many calls.
    fail_stashes_after: usize,
}

impl StashStore for CountingStore {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        let calls = self.stash_calls.fetch_add(1, Ordering::Relaxed);
        if self.fail_stashes_after != 0 && calls >= self.fail_stashes_after {
            return Err(StashError::Backend("simulated mid-run failure".to_owned()));
        }
        self.inner.stash(payload)
    }

    fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
        self.inner.retrieve(hash)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        self.inner.evict_expired()
    }

    fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError> {
        self.delete_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.delete(hash, generation)
    }
}

fn request(content: &str) -> PostToolRequest {
    PostToolRequest {
        result_kind: ResultKind::Tool,
        tool_name: "Bash".into(),
        content: content.into(),
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
    }
}

fn config(timeout: Duration, truncate_arrays_at: usize) -> PostToolPipelineConfig {
    PostToolPipelineConfig {
        timeout,
        max_input_bytes: 1024 * 1024,
        min_input_chars: 0,
        error_min_input_chars: DEFAULT_ERROR_MIN_CHARS,
        compression_enabled: true,
        search_path_sharing_enabled: true,
        diff_compression_enabled: false,
        html_extraction_enabled: false,
        embedded_content_enabled: false,
        browser_snapshot_enabled: false,
        stash_enabled: true,
        require_reversibility: false,
        force_json: true,
        allow_toon: false,
        min_toon_chars: usize::MAX,
        mixed_content_enabled: false,
        json: JsonCompressionConfig {
            truncate_arrays_at,
            array_tail_preserve: 0,
            ..JsonCompressionConfig::default()
        },
    }
}

fn record_array(count: usize) -> String {
    serde_json::to_string(
        &(0..count)
            .map(|index| {
                serde_json::json!({
                    "id": index,
                    "message": format!("record-{index}-{}", "x".repeat(80)),
                    "status": "ok"
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn one_json_domain_trace_reaches_one_stash_commit() {
    let input = serde_json::to_string(&serde_json::json!({
        "debug": "diagnostic context",
        "items": (0..12)
            .map(|index| format!("item-{index}-{}", "x".repeat(80)))
            .collect::<Vec<_>>(),
    }))
    .unwrap();
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();

    let run = PostToolPipeline::run(
        &request(&input),
        &config(Duration::from_secs(1), 2),
        Some(&store),
    );

    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.operations, [AppliedOperation::JsonTruncation]);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::JsonTruncation]
    );
    let output: serde_json::Value = serde_json::from_str(&run.response.output).unwrap();
    assert_eq!(output["debug"], "diagnostic context");
    assert_eq!(run.response.stash_keys.len(), 1);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
    assert_eq!(concrete.len(), 1);
}

#[test]
fn rejected_json_candidate_is_arbitrated_and_rolled_back_once() {
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(
        &request(r#"["a","b"]"#),
        &config(Duration::from_secs(1), 1),
        Some(&store),
    );

    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.len(), 0);
}

#[test]
fn a_marked_json_document_is_compressed_not_bypassed() {
    // The detector skips one leading byte-order mark, so the marked page
    // takes the JSON path; the parser must skip it too, or that routing
    // turns a passthrough into a pipeline error. Recovery keeps the
    // marked bytes exactly.
    let plain = serde_json::to_string(&serde_json::json!({
        "debug": "discarded noise",
        "items": (0..12)
            .map(|index| format!("item-{index}-{}", "x".repeat(80)))
            .collect::<Vec<_>>(),
    }))
    .unwrap();
    let input = format!("\u{feff}{plain}");
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let mut config = build_log_config();
    config.json.truncate_arrays_at = 2;
    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));

    assert_eq!(run.response.content_type, Some(ContentType::Json));
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert!(run.response.output.chars().count() < input.chars().count());
    assert!(!run.response.output.contains('\u{feff}'));
    // The JSON domain stashes the truncated tail for replay recovery;
    // the uncompressed twin takes exactly this path, mark aside.
    assert_eq!(run.response.stash_keys.len(), 1);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn record_reduction_has_one_final_commit_and_protocol_operation() {
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(
        &request(&record_array(40)),
        &config(Duration::from_secs(1), 32),
        Some(&store),
    );

    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.operations, [AppliedOperation::JsonRecordReduction]);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::JsonRecordReduction]
    );
    assert_eq!(run.response.recoverability, Recoverability::Retrievable);
    assert_eq!(run.response.stash_keys.len(), 1);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
    assert_eq!(concrete.len(), 1);
}

#[test]
fn record_reduction_dry_run_uses_only_a_temporary_store() {
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let mut dry_run = config(Duration::from_secs(1), 32);
    dry_run.compression_enabled = false;
    dry_run.require_reversibility = true;

    let input = record_array(40);
    let run = PostToolPipeline::run(&request(&input), &dry_run, Some(&store));

    assert_eq!(run.response.disposition, Disposition::DryRun);
    assert_eq!(run.response.output, input);
    assert_eq!(run.operations, [AppliedOperation::JsonRecordReduction]);
    assert!(run.response.after_tokens < run.response.before_tokens);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
    assert_eq!(concrete.len(), 0);
}

#[test]
fn record_candidate_with_no_savings_rolls_back_its_single_write() {
    let input = serde_json::to_string(&vec![serde_json::json!({}); 33]).unwrap();
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let config = config(Duration::from_secs(1), 32);

    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));

    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(run.response.output, input);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.len(), 0);
}

#[test]
fn timed_out_json_candidate_is_rolled_back_once() {
    let input = serde_json::to_string(
        &(0..12)
            .map(|index| format!("item-{index}-{}", "x".repeat(80)))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(&request(&input), &config(Duration::ZERO, 2), Some(&store));

    assert_eq!(run.response.disposition, Disposition::Timeout);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.len(), 0);
}

#[test]
fn dry_run_reports_recoverability_for_the_emitted_original() {
    let input = serde_json::to_string(
        &(0..12)
            .map(|index| format!("item-{index}-{}", "x".repeat(80)))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut dry_run = config(Duration::from_secs(1), 2);
    dry_run.compression_enabled = false;
    dry_run.stash_enabled = false;

    let run = PostToolPipeline::run(&request(&input), &dry_run, None);

    assert_eq!(run.response.disposition, Disposition::DryRun);
    assert_eq!(run.response.output, input);
    assert!(run.response.applied_operations.is_empty());
    assert_eq!(run.response.recoverability, Recoverability::Lossless);
    assert!(run.response.after_tokens < run.response.before_tokens);
}

fn build_log() -> String {
    let mut output = "$ cargo build\n".to_owned();
    for index in 0..30 {
        output.push_str(&format!(
            "Compiling package-{index:03} v0.1.{index} with extended progress output\n"
        ));
    }
    output.push_str("Finished `dev` profile [unoptimized] target(s) in 1.2s\n");
    output
}

fn go_test_log() -> String {
    (0..30)
        .map(|index| format!("ok  \tgithub.com/acme/pkg{index:02}\t0.{index:03}s\n"))
        .collect()
}

fn build_log_with_midway_diagnostic() -> String {
    // One cargo run split by a diagnostic into two reducible blocks, so
    // the compressor performs two stash writes.
    let mut output = "$ cargo build\n".to_owned();
    for index in 0..30 {
        if index == 15 {
            output.push_str("error[E0308]: mismatched types\n  --> src/main.rs:3:4\n");
        }
        output.push_str(&format!(
            "Compiling package-{index:03} v0.1.{index} with extended progress output\n"
        ));
    }
    output.push_str("Finished `dev` profile [unoptimized] target(s) in 1.2s\n");
    output
}

fn build_log_with_duplicate_blocks() -> String {
    // Two routine blocks whose omitted payloads are identical, so the
    // compressor creates then refreshes a single stash row.
    let mut output = "$ cargo build\n".to_owned();
    for group in 0..2 {
        for _ in 0..12 {
            output.push_str("Compiling package v0.1.0 with extended progress output\n");
        }
        output.push_str(&format!("phase boundary {group}\n"));
    }
    output.push_str("Finished `dev` profile [unoptimized] target(s) in 1.2s\n");
    output
}

fn build_log_config() -> PostToolPipelineConfig {
    let mut config = config(Duration::from_secs(1), 32);
    config.force_json = false;
    config.require_reversibility = true;
    config
}

fn diff_input(context_width: usize) -> String {
    format!(
        "diff --git a/f b/f\nindex 123..456 100644\n--- a/f\n+++ b/f\n@@ -1,12 +1,12 @@\n{}-old\n+new\n tail\n",
        format!(" {}\n", "x".repeat(context_width)).repeat(10)
    )
}

#[test]
fn diff_respects_disable_and_requires_recovery_and_command_text() {
    let input = diff_input(100);
    for mode in 0..11 {
        let mut req = request(&input);
        let mut config = build_log_config();
        config.diff_compression_enabled = true;
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        match mode {
            0 => config.diff_compression_enabled = false,
            1 => config.stash_enabled = false,
            2 => req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None,
            3 => req.capabilities.replace_output = false,
            4 => req.capabilities.replace_with_text = false,
            5 => req.content_origin = ContentOrigin::FileContent,
            6 => req.content_origin = ContentOrigin::ApiResponse,
            7 => req.tool_name = "Grep".into(),
            8 => {
                req.status = ToolResultStatus::Error;
                req.exit_code = Some(127);
            }
            9 => config.max_input_bytes = input.len() - 1,
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
fn diff_retains_changes_and_recovers_original_with_one_stash_write() {
    let input = diff_input(100);
    for origin in [ContentOrigin::CommandOutput, ContentOrigin::FileRead] {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut config = build_log_config();
        config.diff_compression_enabled = true;
        let mut req = request(&input);
        req.content_origin = origin;
        let run = PostToolPipeline::run(&req, &config, Some(&store));

        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::DiffReduction]
        );
        assert_eq!(run.response.recoverability, Recoverability::Retrievable);
        assert!(run.response.output.contains("-old\n+new\n tail\n"));
        assert!(run.response.output.contains("@@ -9,4 +9,4 @@"));
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
fn diff_rejection_rolls_back_and_dry_run_uses_temporary_stash() {
    for mode in 0..4 {
        let input = diff_input(if mode == 0 { 1 } else { 100 });
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut config = build_log_config();
        config.diff_compression_enabled = true;
        let mut req = request(&input);
        let (disposition, writes, deletes) = match mode {
            0 => (Disposition::NoSavings, 1, 1),
            1 => {
                config.timeout = Duration::ZERO;
                (Disposition::Timeout, 1, 1)
            }
            2 => {
                config.compression_enabled = false;
                (Disposition::DryRun, 0, 0)
            }
            3 => {
                req.content.push_str("[Output truncated by host]\n");
                (Disposition::NoSavings, 0, 0)
            }
            _ => unreachable!(),
        };
        let run = PostToolPipeline::run(&req, &config, Some(&store));
        assert_eq!(run.response.disposition, disposition, "mode {mode}");
        assert_eq!(run.response.output, req.content);
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), writes);
        assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), deletes);
        assert_eq!(concrete.len(), 0);
        if mode == 2 {
            assert_eq!(run.operations, [AppliedOperation::DiffReduction]);
            assert!(run.response.before_tokens - run.response.after_tokens >= 16);
        }
    }
}

#[test]
fn diff_stash_failure_degrades_to_unchanged_and_counts_one_error() {
    // The stash backend, not the diff renderer, fails here (disk full,
    // read-only state). The pipeline must degrade to the unchanged
    // candidate and surface the failure through the stash-error counter
    // — like the JSON, build-log and tabular domains — so the lifecycle
    // reports RuntimeError::StashWrite instead of failing the whole
    // PostTool call as RuntimeError::Pipeline.
    let input = diff_input(100);
    let mut config = build_log_config();
    config.diff_compression_enabled = true;
    let mut req = request(&input);
    req.content_origin = ContentOrigin::CommandOutput;
    // CountingStore with the counter pre-advanced past the threshold so
    // the very first `stash` call fails, matching the FailingStore this
    // replaces without a third copy of the double.
    let store: Arc<dyn StashStore> = Arc::new(CountingStore {
        stash_calls: AtomicUsize::new(1),
        fail_stashes_after: 1,
        ..CountingStore::default()
    });
    let run = PostToolPipeline::run(&req, &config, Some(&store));

    assert_eq!(run.response.output, input);
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert!(run.response.applied_operations.is_empty());
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(run.stash_errors, Some(1));
    assert_eq!(run.stash_writes, Some(0));
}

const MAVEN_LOG: &str = include_str!(
    "../../../../tokenless-compressors/tests/fixtures/build_logs/maven/jsoup_batch.txt"
);

#[test]
fn maven_layout_routes_without_recovery_and_never_writes_stash() {
    for mode in 0..4 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(MAVEN_LOG);
        let mut config = build_log_config();
        if mode == 0 {
            req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
        }
        if mode == 1 {
            config.stash_enabled = false;
        }
        if mode == 3 {
            req.content_origin = ContentOrigin::FileRead;
        }
        let run = PostToolPipeline::run(&req, &config, if mode == 2 { None } else { Some(&store) });
        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
        assert_eq!(run.response.recoverability, Recoverability::Lossless);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::BuildLogReduction]
        );
        assert!(run.response.after_tokens < run.response.before_tokens);
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn maven_obeys_existing_source_status_replacement_and_acceptance_guards() {
    for mode in 0..8 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(MAVEN_LOG);
        let mut config = build_log_config();
        match mode {
            0 => {
                req.status = ToolResultStatus::Error;
                req.exit_code = Some(127);
            }
            1 => req.capabilities.replace_output = false,
            2 => req.content_origin = ContentOrigin::FileContent,
            3 => req.content_origin = ContentOrigin::ApiResponse,
            4 => config.min_input_chars = usize::MAX,
            5 => config.compression_enabled = false,
            6 => config.timeout = Duration::ZERO,
            _ => req.command = Some("rg needle build.log".into()),
        }
        let run = PostToolPipeline::run(&req, &config, Some(&store));
        assert_eq!(run.response.output, MAVEN_LOG, "mode {mode}");
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
        if mode == 5 {
            assert_eq!(run.response.disposition, Disposition::DryRun);
        }
        if mode == 6 {
            assert_eq!(run.response.disposition, Disposition::Timeout);
        }
    }
}

const PNPM_INSTALL: &str = include_str!(
    "../../../../tokenless-compressors/tests/fixtures/build_logs/pnpm_install_live.txt"
);

const GRADLE_LOG: &str = include_str!(
    "../../../../tokenless-compressors/tests/fixtures/build_logs/gradle/resilience4j_incremental.txt"
);

const CTEST_LOG: &str = include_str!(
    "../../../../tokenless-compressors/tests/fixtures/build_logs/ctest/brotli_serial.txt"
);

const CMAKE_BUILD_LOG: &str = include_str!(
    "../../../../tokenless-compressors/tests/fixtures/build_logs/cmake_build/brotli_ninja_cold.txt"
);

#[test]
fn cmake_build_layout_routes_without_recovery_and_never_writes_stash() {
    for mode in 0..4 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(CMAKE_BUILD_LOG);
        let mut config = build_log_config();
        if mode == 0 {
            req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
        }
        if mode == 1 {
            config.stash_enabled = false;
        }
        if mode == 3 {
            req.content_origin = ContentOrigin::FileRead;
        }
        let run = PostToolPipeline::run(&req, &config, if mode == 2 { None } else { Some(&store) });
        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
        assert_eq!(run.response.recoverability, Recoverability::Lossless);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::BuildLogReduction]
        );
        assert!(run.response.after_tokens < run.response.before_tokens);
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn cmake_build_obeys_existing_source_status_replacement_and_acceptance_guards() {
    for mode in 0..8 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(CMAKE_BUILD_LOG);
        let mut config = build_log_config();
        match mode {
            0 => {
                req.status = ToolResultStatus::Error;
                req.exit_code = Some(127);
            }
            1 => req.capabilities.replace_output = false,
            2 => req.content_origin = ContentOrigin::FileContent,
            3 => req.content_origin = ContentOrigin::ApiResponse,
            4 => config.min_input_chars = usize::MAX,
            5 => config.compression_enabled = false,
            6 => config.timeout = Duration::ZERO,
            _ => req.command = Some("rg needle build.log".into()),
        }
        let run = PostToolPipeline::run(&req, &config, Some(&store));
        assert_eq!(run.response.output, CMAKE_BUILD_LOG, "mode {mode}");
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
        if mode == 5 {
            assert_eq!(run.response.disposition, Disposition::DryRun);
        }
        if mode == 6 {
            assert_eq!(run.response.disposition, Disposition::Timeout);
        }
    }
}

const CC_COMPILE_LOG: &str = include_str!(
    "../../../../tokenless-compressors/tests/fixtures/build_logs/cc_compile/brotli_make_verbose.txt"
);

const ZSTD_COMPILE_LOG: &str = include_str!(
    "../../../../tokenless-compressors/tests/fixtures/build_logs/cc_compile/zstd_make_lib_v1_gcc.txt"
);

#[test]
fn cc_compile_layout_routes_without_recovery_and_never_writes_stash() {
    for log in [CC_COMPILE_LOG, ZSTD_COMPILE_LOG] {
        for mode in 0..4 {
            let concrete = Arc::new(CountingStore::default());
            let store: Arc<dyn StashStore> = concrete.clone();
            let mut req = request(log);
            let mut config = build_log_config();
            if mode == 0 {
                req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
            }
            if mode == 1 {
                config.stash_enabled = false;
            }
            if mode == 3 {
                req.content_origin = ContentOrigin::FileRead;
            }
            let run =
                PostToolPipeline::run(&req, &config, if mode == 2 { None } else { Some(&store) });
            assert_eq!(run.response.disposition, Disposition::Applied);
            assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
            assert_eq!(run.response.recoverability, Recoverability::Lossless);
            assert_eq!(
                run.response.applied_operations,
                [AppliedOperation::BuildLogReduction]
            );
            assert!(run.response.after_tokens < run.response.before_tokens);
            assert!(run.response.stash_keys.is_empty());
            assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
        }
    }
}

#[test]
fn cc_compile_obeys_existing_source_status_replacement_and_acceptance_guards() {
    for mode in 0..8 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(CC_COMPILE_LOG);
        let mut config = build_log_config();
        match mode {
            0 => req.status = ToolResultStatus::Error,
            1 => req.capabilities.replace_output = false,
            2 => req.content_origin = ContentOrigin::FileContent,
            3 => req.content_origin = ContentOrigin::ApiResponse,
            4 => config.min_input_chars = usize::MAX,
            5 => config.compression_enabled = false,
            6 => config.timeout = Duration::ZERO,
            _ => req.command = Some("rg needle build.log".into()),
        }
        let run = PostToolPipeline::run(&req, &config, Some(&store));
        assert_eq!(run.response.output, CC_COMPILE_LOG, "mode {mode}");
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
        if mode == 5 {
            assert_eq!(run.response.disposition, Disposition::DryRun);
        }
        if mode == 6 {
            assert_eq!(run.response.disposition, Disposition::Timeout);
        }
    }
}

#[test]
fn ctest_layout_routes_without_recovery_and_never_writes_stash() {
    for mode in 0..4 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(CTEST_LOG);
        let mut config = build_log_config();
        if mode == 0 {
            req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
        }
        if mode == 1 {
            config.stash_enabled = false;
        }
        if mode == 3 {
            req.content_origin = ContentOrigin::FileRead;
        }
        let run = PostToolPipeline::run(&req, &config, if mode == 2 { None } else { Some(&store) });
        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
        assert_eq!(run.response.recoverability, Recoverability::Lossless);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::BuildLogReduction]
        );
        assert!(run.response.after_tokens < run.response.before_tokens);
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn ctest_obeys_existing_source_status_replacement_and_acceptance_guards() {
    for mode in 0..8 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(CTEST_LOG);
        let mut config = build_log_config();
        match mode {
            0 => {
                req.status = ToolResultStatus::Error;
                req.exit_code = Some(127);
            }
            1 => req.capabilities.replace_output = false,
            2 => req.content_origin = ContentOrigin::FileContent,
            3 => req.content_origin = ContentOrigin::ApiResponse,
            4 => config.min_input_chars = usize::MAX,
            5 => config.compression_enabled = false,
            6 => config.timeout = Duration::ZERO,
            _ => req.command = Some("rg needle ctest.log".into()),
        }
        let run = PostToolPipeline::run(&req, &config, Some(&store));
        assert_eq!(run.response.output, CTEST_LOG, "mode {mode}");
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
        if mode == 5 {
            assert_eq!(run.response.disposition, Disposition::DryRun);
        }
        if mode == 6 {
            assert_eq!(run.response.disposition, Disposition::Timeout);
        }
    }
}

#[test]
fn gradle_layout_routes_without_recovery_and_never_writes_stash() {
    for mode in 0..4 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(GRADLE_LOG);
        let mut config = build_log_config();
        if mode == 0 {
            req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
        }
        if mode == 1 {
            config.stash_enabled = false;
        }
        if mode == 3 {
            req.content_origin = ContentOrigin::FileRead;
        }
        let run = PostToolPipeline::run(&req, &config, if mode == 2 { None } else { Some(&store) });
        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
        assert_eq!(run.response.recoverability, Recoverability::Lossless);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::BuildLogReduction]
        );
        assert!(run.response.after_tokens < run.response.before_tokens);
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn gradle_obeys_existing_source_status_replacement_and_acceptance_guards() {
    for mode in 0..8 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(GRADLE_LOG);
        let mut config = build_log_config();
        match mode {
            0 => {
                req.status = ToolResultStatus::Error;
                req.exit_code = Some(127);
            }
            1 => req.capabilities.replace_output = false,
            2 => req.content_origin = ContentOrigin::FileContent,
            3 => req.content_origin = ContentOrigin::ApiResponse,
            4 => config.min_input_chars = usize::MAX,
            5 => config.compression_enabled = false,
            6 => config.timeout = Duration::ZERO,
            _ => req.command = Some("rg needle build.log".into()),
        }
        let run = PostToolPipeline::run(&req, &config, Some(&store));
        assert_eq!(run.response.output, GRADLE_LOG, "mode {mode}");
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
        if mode == 5 {
            assert_eq!(run.response.disposition, Disposition::DryRun);
        }
        if mode == 6 {
            assert_eq!(run.response.disposition, Disposition::Timeout);
        }
    }
}

#[test]
fn pnpm_layout_is_lossless_without_recovery_or_storage() {
    for mode in 0..3 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(PNPM_INSTALL);
        let mut config = build_log_config();
        if mode == 0 {
            req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
        } else if mode == 1 {
            config.stash_enabled = false;
        }
        let run = PostToolPipeline::run(&req, &config, if mode == 2 { None } else { Some(&store) });
        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
        assert_eq!(run.response.recoverability, Recoverability::Lossless);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::BuildLogReduction]
        );
        assert_eq!(run.response.before_tokens, 332);
        assert_eq!(run.response.after_tokens, 256);
        assert_eq!(run.response.output.matches("Progress:").count(), 13);
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn pnpm_combines_layout_and_progress_in_one_committed_candidate() {
    for origin in [ContentOrigin::CommandOutput, ContentOrigin::FileRead] {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(PNPM_INSTALL);
        req.content_origin = origin;
        let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.recoverability, Recoverability::Retrievable);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::BuildLogReduction]
        );
        assert_eq!(run.response.after_tokens, 184);
        assert_eq!(run.response.stash_keys.len(), 1);
        let payload = concrete
            .retrieve(&run.response.stash_keys[0])
            .unwrap()
            .unwrap();
        assert_eq!(
            payload,
            PNPM_INSTALL
                .lines()
                .skip(12)
                .take(7)
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        );
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
        assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
        assert_eq!(concrete.len(), 1);
    }
}

#[test]
fn pnpm_dry_run_and_timeout_do_not_leave_persistent_omissions() {
    for dry_run in [true, false] {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut config = build_log_config();
        if dry_run {
            config.compression_enabled = false;
        } else {
            config.timeout = Duration::ZERO;
        }
        let run = PostToolPipeline::run(&request(PNPM_INSTALL), &config, Some(&store));
        assert_eq!(run.response.output, PNPM_INSTALL);
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
        assert_eq!(concrete.len(), 0);
        assert_eq!(
            concrete.stash_calls.load(Ordering::Relaxed),
            usize::from(!dry_run)
        );
        assert_eq!(
            concrete.delete_calls.load(Ordering::Relaxed),
            usize::from(!dry_run)
        );
        if dry_run {
            assert_eq!(run.response.after_tokens, 184);
        }
    }
}

#[test]
fn pnpm_partial_stash_failure_retains_layout_and_rolls_back_progress() {
    let concrete = Arc::new(CountingStore {
        fail_stashes_after: 1,
        ..CountingStore::default()
    });
    let store: Arc<dyn StashStore> = concrete.clone();
    let input = PNPM_INSTALL.repeat(2);
    let run = PostToolPipeline::run(&request(&input), &build_log_config(), Some(&store));
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.response.recoverability, Recoverability::Lossless);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::BuildLogReduction]
    );
    assert_eq!(run.response.output.matches("Progress:").count(), 26);
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(run.stash_errors, Some(1));
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 2);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.len(), 0);
}

#[test]
fn one_build_log_domain_reaches_one_final_commit() {
    for origin in [ContentOrigin::CommandOutput, ContentOrigin::FileRead] {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(&build_log());
        req.content_origin = origin;
        let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store));

        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
        assert_eq!(run.operations, [AppliedOperation::BuildLogReduction]);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::BuildLogReduction]
        );
        assert_eq!(run.response.recoverability, Recoverability::Retrievable);
        assert_eq!(run.response.stash_keys.len(), 1);
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
        assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
        assert_eq!(concrete.len(), 1);
    }
}

#[test]
fn partial_build_log_stash_failure_deletes_orphaned_tentative_writes() {
    // The compressor's second stash write fails mid-run: it returns the
    // lossless original plus one tentative write, and the pipeline must
    // delete that orphaned row instead of leaving an unreachable entry.
    let concrete = Arc::new(CountingStore {
        fail_stashes_after: 1,
        ..CountingStore::default()
    });
    let store: Arc<dyn StashStore> = concrete.clone();
    let input = build_log_with_midway_diagnostic();

    let run = PostToolPipeline::run(&request(&input), &build_log_config(), Some(&store));

    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(run.response.output, input);
    assert!(run.response.applied_operations.is_empty());
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(run.stash_errors, Some(1));
    assert_eq!(run.stash_writes, Some(0));
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 2);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 1);
    assert_eq!(concrete.len(), 0);
}

#[test]
fn applied_build_log_with_duplicate_payloads_commits_one_row() {
    // Two omission blocks share one payload: the compressor creates then
    // refreshes one row, both markers carry its key, and commit reports
    // the key exactly once with a single surviving row.
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let input = build_log_with_duplicate_blocks();

    let run = PostToolPipeline::run(&request(&input), &build_log_config(), Some(&store));

    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.response.recoverability, Recoverability::Retrievable);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::BuildLogReduction]
    );
    assert_eq!(run.response.stash_keys.len(), 1);
    assert_eq!(run.stash_writes, Some(1));
    assert_eq!(run.stash_errors, Some(0));
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 2);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
    assert_eq!(concrete.len(), 1);
}

#[test]
fn native_go_test_rows_reach_build_log_reduction() {
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(&request(&go_test_log()), &build_log_config(), Some(&store));

    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
    assert_eq!(run.operations, [AppliedOperation::BuildLogReduction]);
    assert_eq!(run.response.recoverability, Recoverability::Retrievable);
    assert_eq!(run.response.stash_keys.len(), 1);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn build_log_dry_run_uses_only_the_temporary_store() {
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let mut config = build_log_config();
    config.compression_enabled = false;
    let input = build_log();

    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));

    assert_eq!(run.response.disposition, Disposition::DryRun);
    assert_eq!(run.response.output, input);
    assert_eq!(run.operations, [AppliedOperation::BuildLogReduction]);
    assert!(run.response.after_tokens < run.response.before_tokens);
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn build_log_from_non_command_origin_passes_through() {
    let mut request = request(&build_log());
    request.content_origin = ContentOrigin::ApiResponse;
    let run = PostToolPipeline::run(&request, &build_log_config(), None);
    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
    assert!(run.operations.is_empty());
}

#[test]
fn ninja_verbose_build_log_ending_in_warning_flag_reaches_build_log_compression() {
    let mut input = (1..=10)
        .map(|i| {
            format!(
                "[{i}/10] /usr/bin/cc -O3 -Wall -DCOMMON -I/work/include -MD -MT obj/u_{i:02}.c.o -MF obj/u_{i:02}.c.o.d -o obj/u_{i:02}.c.o -c /work/src/u_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let warning = "/work/src/u_10.c:12:5: warning: unused variable 'x' [-Wunused-variable]\n";
    input.push_str(warning);

    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(&request(&input), &build_log_config(), Some(&store));

    assert_eq!(run.response.content_type, Some(ContentType::BuildLog));
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.operations, [AppliedOperation::BuildLogReduction]);
    assert!(run.response.output.contains(warning));
    assert!(run.response.after_tokens < run.response.before_tokens);
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn empty_first_json_lines_passthrough_with_mixed_enabled_and_padded_json_stay_json() {
    let mut mixed_cfg = build_log_config();
    mixed_cfg.mixed_content_enabled = true;
    mixed_cfg.allow_toon = true;
    mixed_cfg.min_toon_chars = 0;

    let array_jsonl = format!("[]\n{}\n", record_array(40));
    let object_jsonl = format!("{{}}\n{{\"items\": {}}}\n", record_array(40));
    let partial_kw_jsonl = format!(
        "[1{}]\ntrue\n[]\n",
        " ".repeat(crate::post_tool::content::MAX_SCAN_BYTES - 5)
    );
    let partial_exp_jsonl = format!(
        "[1{}]\n1e+5\n[]\n",
        " ".repeat(crate::post_tool::content::MAX_SCAN_BYTES - 7)
    );
    for input in [
        &array_jsonl,
        &object_jsonl,
        &partial_kw_jsonl,
        &partial_exp_jsonl,
    ] {
        let run = PostToolPipeline::run(&request(input), &mixed_cfg, None);
        assert_eq!(run.response.content_type, Some(ContentType::Json));
        assert_eq!(run.response.disposition, Disposition::Passthrough);
        assert_eq!(&run.response.output, input);
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
    }

    let padded = format!(
        "{{{}\"k\": 1}}",
        " ".repeat(crate::post_tool::content::MAX_SCAN_BYTES)
    );
    let padded_cfg = config(Duration::from_secs(1), 2);
    let run = PostToolPipeline::run(&request(&padded), &padded_cfg, None);
    assert_eq!(run.response.content_type, Some(ContentType::Json));
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.response.output, "{\"k\":1}");
}

#[test]
fn quoted_json_scalar_passes_through() {
    let input = serde_json::to_string(&"x".repeat(5_000)).unwrap();
    let mut config = config(Duration::from_secs(1), 2);
    config.force_json = false;

    let run = PostToolPipeline::run(&request(&input), &config, None);

    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.output, input);
    assert!(run.operations.is_empty());
}

#[test]
fn oversized_passthrough_identifies_the_byte_estimator() {
    let input = "界".repeat(4);
    let mut config = config(Duration::from_secs(1), 2);
    config.max_input_bytes = input.len() - 1;

    let run = PostToolPipeline::run(&request(&input), &config, None);

    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.before_tokens, 3);
    assert_eq!(run.response.after_tokens, 3);
    assert_ne!(run.response.before_tokens, estimate_tokens(&input) as u64);
    assert_eq!(run.response.tokenizer_id, BYTE_ESTIMATOR_ID);
}

#[test]
fn error_ci_gate_and_level_e1_pass_through() {
    let input = build_log_with_midway_diagnostic();
    for recovery in [
        tokenless_protocol::RecoveryMethod::None,
        tokenless_protocol::RecoveryMethod::Shell,
    ] {
        let mut gate_req = request(&input);
        gate_req.status = ToolResultStatus::Error;
        gate_req.exit_code = None;
        gate_req.capabilities.recovery = recovery;
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let gated = PostToolPipeline::run(&gate_req, &build_log_config(), Some(&store));
        assert_eq!(gated.response.disposition, Disposition::Passthrough);
        assert_eq!(gated.response.output, input);
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }

    for exit_code in [126, 127, 137] {
        let mut req = request(&input);
        req.status = ToolResultStatus::Error;
        req.exit_code = Some(exit_code);
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
        assert_eq!(
            run.response.disposition,
            Disposition::Passthrough,
            "exit_code={exit_code}"
        );
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }

    let mut custom_floor = build_log_config();
    custom_floor.error_min_input_chars = input.chars().count() + 1;
    let mut floored_req = request(&input);
    floored_req.status = ToolResultStatus::Error;
    floored_req.exit_code = Some(1);
    let floored_run = PostToolPipeline::run(&floored_req, &custom_floor, None);
    assert_eq!(floored_run.response.disposition, Disposition::Passthrough);
    assert_eq!(floored_run.response.output, input);

    let short_error =
        "error[E0308]: mismatched types\n  --> src/main.rs:3:4\ntest result: FAILED.\n";
    let mut short_req = request(short_error);
    short_req.status = ToolResultStatus::Error;
    short_req.exit_code = Some(101);
    let short_run = PostToolPipeline::run(&short_req, &build_log_config(), None);
    assert_eq!(short_run.response.disposition, Disposition::Passthrough);
    assert_eq!(short_run.response.output, short_error);
}

#[test]
fn level_e2_failing_cargo_pytest_and_jest_save_at_least_fifty_five_percent_and_preserve_facts() {
    let mut cargo_fail = String::from("$ cargo test\n");
    for i in 0..40 {
        cargo_fail.push_str(&format!(
            "   Compiling worker-crate-{i:02} v0.1.{i} (/workspace/packages/worker-crate-{i:02}) with extended telemetry output\n"
        ));
    }
    for i in 0..45 {
        cargo_fail.push_str(&format!(
            "test module_{i:02}::validates_extended_protocol_invariant_case_{i:02} ... ok\n"
        ));
    }
    cargo_fail.push_str(
        "test parser::rejects_invalid_token ... FAILED\n\
         failures:\n\
         ---- parser::rejects_invalid_token stdout ----\n\
         thread 'parser::rejects_invalid_token' panicked at src/parser.rs:88:13:\n\
         assertion `left == right` failed\n\
         error[E0308]: mismatched types\n\
         --> src/parser.rs:88:13\n\
         test result: FAILED. 45 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
    );

    let mut pytest_fail = String::from(
        "$ pytest\n============================= test session starts ==============================\n",
    );
    for i in 0..50 {
        pytest_fail.push_str(&format!(
            "tests/suite_{i:02}/test_pipeline_contract_{i:02}.py::test_validates_stream_record PASSED [ {:>2}%]\n",
            (i * 2).min(99)
        ));
    }
    pytest_fail.push_str(
        "tests/suite_99/test_error_mode.py::test_keeps_failure FAILED [100%]\n\
         =================================== FAILURES ===================================\n\
         _____________________________ test_keeps_failure _______________________________\n\
         File \"tests/suite_99/test_error_mode.py\", line 42, in test_keeps_failure\n\
         E   AssertionError: expected compressed log to keep diagnostics\n\
         =========================== short test summary info ============================\n\
         FAILED tests/suite_99/test_error_mode.py::test_keeps_failure - AssertionError\n\
         ====== 1 failed, 50 passed in 0.84s ======\n",
    );

    let mut jest_fail = String::from("$ jest\n");
    for i in 0..45 {
        jest_fail.push_str(&format!(
            "PASS src/components/widget_{i:02}/render_and_validate_state.test.ts\n"
        ));
    }
    jest_fail.push_str(
        "FAIL src/components/critical/auth.test.ts\n\
         ● auth guard › rejects expired token\n\
           Expected: 401\n\
           Received: 200\n\
             at Object.<anonymous> (src/components/critical/auth.test.ts:19:5)\n\
         Test Suites: 1 failed, 45 passed, 46 total\n\
         Tests:       1 failed, 91 passed, 92 total\n",
    );

    for (name, input, exit_code, required) in [
        (
            "cargo_fail",
            cargo_fail,
            101,
            vec![
                "parser::rejects_invalid_token",
                "error[E0308]",
                "src/parser.rs:88",
                "test result: FAILED. 45 passed; 1 failed",
            ],
        ),
        (
            "pytest_fail",
            pytest_fail,
            1,
            vec![
                "tests/suite_99/test_error_mode.py::test_keeps_failure",
                "AssertionError",
                "File \"tests/suite_99/test_error_mode.py\", line 42",
                "====== 1 failed, 50 passed in 0.84s ======",
            ],
        ),
        (
            "jest_fail",
            jest_fail,
            1,
            vec![
                "auth guard › rejects expired token",
                "src/components/critical/auth.test.ts:19",
                "Test Suites: 1 failed, 45 passed, 46 total",
                "Tests:       1 failed, 91 passed, 92 total",
            ],
        ),
    ] {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut req = request(&input);
        req.status = ToolResultStatus::Error;
        req.exit_code = Some(exit_code);
        let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
        assert_eq!(run.response.disposition, Disposition::Applied, "{name}");
        assert_eq!(
            run.response.content_type,
            Some(ContentType::BuildLog),
            "{name}"
        );
        let savings_pct = (run.response.before_tokens - run.response.after_tokens) * 100
            / run.response.before_tokens;
        assert!(
            savings_pct >= 55,
            "{name}: expected >= 55% savings, got {savings_pct}% ({} -> {})",
            run.response.before_tokens,
            run.response.after_tokens
        );
        assert!(!run.response.stash_keys.is_empty(), "{name}");
        for marker in required {
            assert!(
                run.response.output.contains(marker),
                "{name}: missing marker `{marker}` in:\n{}",
                run.response.output
            );
        }
    }
}

#[test]
fn error_fact_guard_rolls_back_stash_when_truncation_drops_diagnostic_code() {
    let input = serde_json::to_string(&serde_json::json!([
        {"msg": format!("padding-0-{}", "x".repeat(200))},
        {"msg": format!("padding-1-{}", "x".repeat(200))},
        {"msg": format!("error[E0308] dropped tail-{}", "x".repeat(200))}
    ]))
    .unwrap();
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let mut cfg = config(Duration::from_secs(1), 1);
    cfg.min_input_chars = 0;
    let mut req = request(&input);
    req.status = ToolResultStatus::Error;
    req.exit_code = Some(1);
    let run = PostToolPipeline::run(&req, &cfg, Some(&store));
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(
        run.fact_guard_violation,
        Some(super::super::FactViolation::DiagnosticCode)
    );
    assert_eq!(run.response.output, input);
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(concrete.len(), 0);
    assert_eq!(
        concrete.stash_calls.load(Ordering::Relaxed),
        concrete.delete_calls.load(Ordering::Relaxed)
    );
}

#[test]
fn error_fact_guard_blocks_json_record_reduction_when_failing_test_or_assertion_drops() {
    let mut records = (0..182)
        .map(|i| {
            serde_json::json!({
                "id": i,
                "status": "ok",
                "msg": format!("routine step {i:03} completed cleanly")
            })
        })
        .collect::<Vec<_>>();
    let test_fact = "test api::rejects_bad_input ... FAILED";
    let assertion_fact = "AssertionError: expected 400, received 200 for malformed request";
    records[90] = serde_json::json!({"id": 90, "status": "ok", "msg": test_fact});
    records[91] = serde_json::json!({"id": 91, "status": "ok", "msg": assertion_fact});
    let input = serde_json::to_string(&records).unwrap();

    // Even when TOON is enabled (and selected for large arrays), facts never disappear.
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let mut cfg = build_log_config();
    cfg.allow_toon = true;
    cfg.min_toon_chars = 0;
    let mut req = request(&input);
    req.status = ToolResultStatus::Error;
    req.exit_code = Some(1);
    let toon_run = PostToolPipeline::run(&req, &cfg, Some(&store));
    if toon_run.response.disposition == Disposition::Applied {
        assert!(toon_run.response.output.contains(test_fact));
        assert!(toon_run.response.output.contains(assertion_fact));
    } else {
        assert_eq!(toon_run.response.disposition, Disposition::NoSavings);
        assert!(toon_run.fact_guard_violation.is_some());
        assert_eq!(toon_run.response.output, input);
        assert_eq!(concrete.len(), 0);
    }

    // Without TOON, compact JSON offers zero savings so RecordReduction attempts to sample
    // the array; missing either middle diagnostic triggers fail-safe stash rollback.
    let concrete_no_toon = Arc::new(CountingStore::default());
    let store_no_toon: Arc<dyn StashStore> = concrete_no_toon.clone();
    cfg.allow_toon = false;
    let run = PostToolPipeline::run(&req, &cfg, Some(&store_no_toon));
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert!(matches!(
        run.fact_guard_violation,
        Some(
            super::super::FactViolation::FailingTest
                | super::super::FactViolation::DiagnosticSignal
        )
    ));
    assert_eq!(run.response.output, input);
    assert!(run.response.stash_keys.is_empty());
    assert_eq!(concrete_no_toon.len(), 0);
    assert_eq!(
        concrete_no_toon.stash_calls.load(Ordering::Relaxed),
        concrete_no_toon.delete_calls.load(Ordering::Relaxed)
    );
}

#[test]
fn error_fact_guard_strips_sgr_inside_json_leaves_before_record_reduction() {
    let mut records = (0..180)
        .map(|i| {
            serde_json::json!({
                "id": i,
                "status": "ok",
                "msg": format!("routine step {i:03} completed cleanly")
            })
        })
        .collect::<Vec<_>>();
    let plain_err = "RuntimeError: failed to load configuration";
    records[90] = serde_json::json!({
        "id": 90,
        "status": "ok",
        "msg": format!("\u{1b}[31m{plain_err}\u{1b}[0m")
    });
    let input = serde_json::to_string(&records).unwrap();

    for allow_toon in [true, false] {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut cfg = build_log_config();
        cfg.allow_toon = allow_toon;
        cfg.min_toon_chars = 0;
        let mut req = request(&input);
        req.status = ToolResultStatus::Error;
        req.exit_code = Some(1);
        let run = PostToolPipeline::run(&req, &cfg, Some(&store));
        if run.response.disposition == Disposition::Applied {
            assert!(
                run.response.output.contains(plain_err),
                "allow_toon={allow_toon}: missing `{plain_err}`"
            );
        } else {
            assert_eq!(run.response.disposition, Disposition::NoSavings);
            assert_eq!(
                run.fact_guard_violation,
                Some(super::super::FactViolation::DiagnosticSignal)
            );
            assert_eq!(run.response.output, input);
            assert_eq!(concrete.len(), 0);
            assert_eq!(
                concrete.stash_calls.load(Ordering::Relaxed),
                concrete.delete_calls.load(Ordering::Relaxed)
            );
        }
    }
}

#[test]
fn triple_arbitration_rejects_utf8_byte_expansion_even_when_chars_and_tokens_drop() {
    let original = "abcd".repeat(4); // 16 ASCII bytes, 16 chars
    let candidate = "界".repeat(10); // 30 UTF-8 bytes, 10 chars, fewer chars/tokens
    let verdict = super::super::arbitration::decide(&super::super::arbitration::ArbitrationInput {
        original: &original,
        candidate: &candidate,
        has_operations: true,
        min_token_savings: 1,
        recoverability: tokenless_compressors::Recoverability::Lossless,
        require_reversibility: false,
        dry_run: false,
        timed_out: false,
    });
    assert_eq!(
        verdict,
        super::super::arbitration::Verdict::Reject(Disposition::NoSavings)
    );
}
