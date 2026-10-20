fn path_list_input() -> String {
    (0..60)
        .map(|i| format!("/workspace/project with spaces/long_directory/records/file {i}.json\n"))
        .collect()
}

#[test]
fn path_lists_route_by_content_and_origin_without_a_command_name() {
    let input = path_list_input();
    for tool in ["Bash", "run_shell_command", "Glob", "SearchFiles", "Grep"] {
        for origin in [ContentOrigin::CommandOutput, ContentOrigin::ApiResponse] {
            let mut req = request(&input);
            req.tool_name = tool.into();
            req.content_origin = origin;
            req.command = None;
            req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None;
            let concrete = Arc::new(CountingStore::default());
            let store: Arc<dyn StashStore> = concrete.clone();
            let run = PostToolPipeline::run(&req, &build_log_config(), Some(&store));
            assert_eq!(run.response.disposition, Disposition::Applied, "{tool} {origin:?}");
            assert_eq!(run.response.content_type, Some(ContentType::SearchResults));
            assert_eq!(run.response.applied_operations, [AppliedOperation::SearchPathSharing]);
            assert_eq!(run.response.recoverability, Recoverability::Lossless);
            assert!(run.response.stash_keys.is_empty());
            assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
            let (_, encoded) = run.response.output.split_once('\n').unwrap();
            let data: Value = serde_json::from_str(encoded).unwrap();
            let paths = data["paths"].as_array().unwrap();
            assert_eq!(paths.len(), 60);
            let restored: String = paths.iter().map(|path| {
                format!("{}{}{}", data["base"].as_str().unwrap(), path.as_str().unwrap(), data["line_ending"].as_str().unwrap())
            }).collect();
            assert_eq!(restored, input);
        }
    }
}

#[test]
fn path_lists_respect_file_capability_status_and_policy_gates() {
    let input = path_list_input();
    for mode in 0..9 {
        let mut req = request(&input);
        req.content_origin = ContentOrigin::CommandOutput;
        let mut config = build_log_config();
        match mode {
            0 => req.content_origin = ContentOrigin::FileContent,
            1 => req.content_origin = ContentOrigin::FileRead,
            2 => req.status = ToolResultStatus::Error,
            3 => req.capabilities.replace_output = false,
            4 => req.capabilities.replace_with_text = false,
            5 => config.search_path_sharing_enabled = false,
            6 => config.max_input_bytes = input.len() - 1,
            7 => config.min_input_chars = input.chars().count() + 1,
            8 => config.timeout = Duration::ZERO,
            _ => unreachable!(),
        }
        let run = PostToolPipeline::run(&req, &config, None);
        assert_eq!(run.response.output, input, "mode={mode}");
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
    }
}

#[test]
fn path_list_switch_does_not_disable_file_read_json_or_build_logs() {
    for enabled in [false, true] {
        let mut config = build_log_config();
        config.search_path_sharing_enabled = enabled;
        let table: String = (0..100)
            .map(|i| format!("\"src/long_directory/file_{i}.rs\",\"{i:04}\"\r\n"))
            .collect();
        for input in [record_array(100), build_log(), table] {
            let mut req = request(&input);
            req.content_origin = ContentOrigin::FileRead;
            let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
            let run = PostToolPipeline::run(&req, &config, Some(&store));
            assert_eq!(run.response.disposition, Disposition::Applied);
            assert!(!run.response.applied_operations.contains(&AppliedOperation::SearchPathSharing));
        }
    }
}

#[test]
fn path_list_dry_run_measures_without_changing_the_response() {
    let input = path_list_input();
    let mut req = request(&input);
    req.content_origin = ContentOrigin::CommandOutput;
    let mut config = build_log_config();
    config.compression_enabled = false;
    let run = PostToolPipeline::run(&req, &config, None);
    assert_eq!(run.response.disposition, Disposition::DryRun);
    assert_eq!(run.response.output, input);
    assert_eq!(run.operations, [AppliedOperation::SearchPathSharing]);
    assert!(run.response.after_tokens < run.response.before_tokens);
    assert!(run.response.stash_keys.is_empty());
}

#[test]
fn path_list_validation_rejects_the_whole_result_after_the_detection_window() {
    for input in [
        "60\n".to_owned(),
        "a/b\na/c\na/d\n".to_owned(),
        format!("{}---\n755 uploads/\n", path_list_input().repeat(6)),
        format!("{}\0", path_list_input()),
    ] {
        for origin in [ContentOrigin::ApiResponse, ContentOrigin::CommandOutput] {
            let mut req = request(&input);
            req.content_origin = origin;
            let run = PostToolPipeline::run(&req, &build_log_config(), None);
            assert_eq!(run.response.output, input);
            assert!(run.response.applied_operations.is_empty());
        }
    }
}

#[test]
fn path_lists_cross_the_detection_byte_limit_without_losing_records() {
    for (ending, boundary, remaining) in [
        ("\n", " ", "file.json\n"),
        ("\r\n", "\r", "\n"),
        ("\n", "", "中文/file.json\n"),
    ] {
        let head = format!("/workspace/project with spaces/中文/file.json{ending}").repeat(60);
        let prefix = "/workspace/";
        let padding = crate::post_tool::content::MAX_SCAN_BYTES - head.len() - prefix.len() - 1;
        let input = format!("{head}{prefix}{}{boundary}{remaining}{head}", "a".repeat(padding));
        for origin in [ContentOrigin::CommandOutput, ContentOrigin::ApiResponse] {
            let mut req = request(&input);
            req.content_origin = origin;
            let run = PostToolPipeline::run(&req, &build_log_config(), None);
            assert_eq!(run.response.disposition, Disposition::Applied);
            assert_eq!(run.response.content_type, Some(ContentType::SearchResults));
            let (_, encoded) = run.response.output.split_once('\n').unwrap();
            let data: Value = serde_json::from_str(encoded).unwrap();
            let paths = data["paths"].as_array().unwrap();
            assert_eq!(paths.len(), 121);
            let mut restored = paths.iter().map(|path| {
                format!("{}{}", data["base"].as_str().unwrap(), path.as_str().unwrap())
            }).collect::<Vec<_>>().join(data["line_ending"].as_str().unwrap());
            if data["trailing_newline"].as_bool().unwrap() {
                restored.push_str(data["line_ending"].as_str().unwrap());
            }
            assert_eq!(restored, input);

            req.content.push_str(&format!("not a path{ending}"));
            let rejected = PostToolPipeline::run(&req, &build_log_config(), None);
            assert_eq!(rejected.response.output, req.content);
            assert!(rejected.response.applied_operations.is_empty());
        }
    }
}
