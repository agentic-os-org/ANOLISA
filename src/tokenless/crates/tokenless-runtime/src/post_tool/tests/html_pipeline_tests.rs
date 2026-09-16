fn html_input(paragraphs: usize) -> String {
    let body = (0..paragraphs)
        .map(|i| format!("<p>Paragraph {i}: {}</p>", "readable words ".repeat(12)))
        .collect::<String>();
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><title>Doc</title>\
         <link rel=\"canonical\" href=\"https://example.com/doc\">\
         <style>{}</style><script>{}</script></head>\
         <body><header><nav><ul><li><a href=\"/\">Home</a></li><li><a href=\"/docs\">Docs</a></li></ul></nav></header>\
         <main><h1>Title</h1>{body}<script>window.analytics = {{}};</script></main>\
         <footer><p>© Example</p></footer></body></html>",
        "body{margin:0;padding:0;color:#333}".repeat(8),
        "var a = 1; function f() { return a; }".repeat(8)
    )
}

#[test]
fn html_is_opt_in_and_requires_recovery_and_text_replacement() {
    let input = html_input(10);
    for mode in 0..10 {
        let mut req = request(&input);
        let mut config = build_log_config();
        config.html_extraction_enabled = true;
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        match mode {
            0 => config.html_extraction_enabled = false,
            1 => config.stash_enabled = false,
            2 => req.capabilities.recovery = tokenless_protocol::RecoveryMethod::None,
            3 => req.capabilities.replace_output = false,
            4 => req.capabilities.replace_with_text = false,
            5 => req.content_origin = ContentOrigin::FileContent,
            6 => req.tool_name = "Grep".into(),
            7 => req.status = ToolResultStatus::Error,
            8 => config.max_input_bytes = input.len() - 1,
            9 => {}
            _ => unreachable!(),
        }
        let run = PostToolPipeline::run(&req, &config, if mode == 9 { None } else { Some(&store) })
            .unwrap();
        assert_eq!(run.response.output, input, "mode {mode}");
        assert!(run.operations.is_empty(), "mode {mode}");
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn html_renders_from_commands_and_apis_with_one_stash_write() {
    let input = html_input(10);
    for origin in [ContentOrigin::CommandOutput, ContentOrigin::ApiResponse] {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut config = build_log_config();
        config.html_extraction_enabled = true;
        let mut req = request(&input);
        req.content_origin = origin;
        let run = PostToolPipeline::run(&req, &config, Some(&store)).unwrap();

        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(run.response.content_type, Some(ContentType::Html));
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::HtmlExtraction]
        );
        assert_eq!(run.response.recoverability, Recoverability::Retrievable);
        let output = &run.response.output;
        assert!(
            output.starts_with("If needed, run in shell: tokenless retrieve "),
            "{output}"
        );
        assert!(output.contains(
            "\n[HTML page rendered as Markdown; <main> only, 2 nodes outside it omitted; \
             removed 2 script, 1 style. Retrieve original for the full page.]\n\
             Title: Doc\nURL: https://example.com/doc\n# Title\n\nParagraph 0: readable words"
        ));
        assert!(output.ends_with("\n[End page]"));
        assert!(!output.contains("Home"));
        assert!(!output.contains("analytics"));
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
fn html_failures_pass_through_and_rejections_roll_back() {
    for mode in 0..4 {
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut config = build_log_config();
        config.html_extraction_enabled = true;
        let (input, disposition, writes, deletes) = match mode {
            // An application shell renders no body: extraction fails, nothing is stashed.
            0 => (
                "<!DOCTYPE html><html><head><title>App</title></head><body><div id=\"app\"></div>\
                 <script src=\"/bundle.js\"></script></body></html>"
                    .to_owned(),
                Disposition::NoSavings,
                0,
                0,
            ),
            // Plain paragraphs without markup overhead do not save enough.
            1 => (
                format!(
                    "<!DOCTYPE html><html><body><p>{}</p></body></html>",
                    "plain words ".repeat(40)
                ),
                Disposition::NoSavings,
                1,
                1,
            ),
            2 => {
                config.timeout = Duration::ZERO;
                (html_input(10), Disposition::Timeout, 1, 1)
            }
            3 => {
                config.compression_enabled = false;
                (html_input(10), Disposition::DryRun, 0, 0)
            }
            _ => unreachable!(),
        };
        let run = PostToolPipeline::run(&request(&input), &config, Some(&store)).unwrap();
        assert_eq!(run.response.disposition, disposition, "mode {mode}");
        assert_eq!(run.response.output, input);
        assert!(run.response.applied_operations.is_empty());
        assert!(run.response.stash_keys.is_empty());
        assert_eq!(concrete.stash_calls.load(Ordering::Relaxed), writes, "mode {mode}");
        assert_eq!(concrete.delete_calls.load(Ordering::Relaxed), deletes, "mode {mode}");
        assert_eq!(concrete.len(), 0);
        if mode == 3 {
            assert_eq!(run.operations, [AppliedOperation::HtmlExtraction]);
        }
    }
}
