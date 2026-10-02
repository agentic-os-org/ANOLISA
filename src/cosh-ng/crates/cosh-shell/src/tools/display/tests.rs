use super::*;

#[test]
fn bash_simple_command() {
    let info = display_for_tool("Bash", r#"{"command":"ls -la"}"#);
    assert_eq!(info.label, "Bash");
    assert_eq!(info.preview, "$ ls -la");
    assert_eq!(info.color, ToolColor::Execute);
}

#[test]
fn bash_aliases_use_canonical_display() {
    for alias in [
        "shell",
        "run_shell_command",
        "tool shell",
        "tool run_shell_command",
    ] {
        let info = display_for_tool(alias, r#"{"command":"pwd"}"#);
        assert_eq!(info.label, "Bash", "{alias}");
        assert_eq!(info.preview, "$ pwd", "{alias}");
        assert_eq!(info.color, ToolColor::Execute, "{alias}");
    }
}

#[test]
fn bash_dangerous_sudo() {
    let info = display_for_tool("Bash", r#"{"command":"sudo apt install foo"}"#);
    assert_eq!(info.color, ToolColor::Dangerous);
    assert_eq!(info.preview, "$ sudo apt install foo");
}

#[test]
fn bash_dangerous_rm() {
    let info = display_for_tool("Bash", r#"{"command":"rm -rf /tmp/x"}"#);
    assert_eq!(info.color, ToolColor::Dangerous);
}

#[test]
fn bash_dangerous_kill() {
    let info = display_for_tool("Bash", r#"{"command":"kill -9 1234"}"#);
    assert_eq!(info.color, ToolColor::Dangerous);
}

#[test]
fn read_simple() {
    let info = display_for_tool("Read", r#"{"file_path":"/tmp/foo.rs"}"#);
    assert_eq!(info.label, "Read");
    assert_eq!(info.preview, "/tmp/foo.rs");
    assert_eq!(info.color, ToolColor::ReadOnly);
}

#[test]
fn qwen_read_alias_uses_canonical_display() {
    let info = display_for_tool("read_file", r#"{"file_path":"/tmp/foo.rs"}"#);
    assert_eq!(info.label, "Read");
    assert_eq!(info.preview, "/tmp/foo.rs");
    assert_eq!(info.color, ToolColor::ReadOnly);
}

#[test]
fn read_with_range() {
    let info = display_for_tool(
        "Read",
        r#"{"file_path":"/tmp/foo.rs","offset":10,"limit":20}"#,
    );
    assert_eq!(info.preview, "/tmp/foo.rs (lines 10..+20)");
}

#[test]
fn read_with_offset_only() {
    let info = display_for_tool("Read", r#"{"file_path":"/tmp/foo.rs","offset":5}"#);
    assert_eq!(info.preview, "/tmp/foo.rs (from line 5)");
}

#[test]
fn write_tool() {
    let info = display_for_tool("Write", r#"{"file_path":"/tmp/new.rs","content":"hello"}"#);
    assert_eq!(info.label, "Write");
    assert_eq!(info.preview, "/tmp/new.rs (new file)");
    assert_eq!(info.color, ToolColor::Write);
}

#[test]
fn qwen_write_file_alias_uses_canonical_display() {
    let info = display_for_tool(
        "write_file",
        r#"{"file_path":"/tmp/new.html","content":"<html></html>"}"#,
    );
    assert_eq!(info.label, "Write");
    assert_eq!(info.preview, "/tmp/new.html (new file)");
    assert_eq!(info.color, ToolColor::Write);
}

#[test]
fn edit_tool() {
    let info = display_for_tool(
        "Edit",
        r#"{"file_path":"/tmp/x.rs","old_string":"foo","new_string":"bar"}"#,
    );
    assert_eq!(info.label, "Edit");
    assert_eq!(info.preview, "/tmp/x.rs (foo -> bar)");
    assert_eq!(info.color, ToolColor::Write);
}

#[test]
fn grep_tool() {
    let info = display_for_tool("Grep", r#"{"pattern":"TODO","path":"src/"}"#);
    assert_eq!(info.label, "Grep");
    assert_eq!(info.preview, "/TODO/ in src/");
    assert_eq!(info.color, ToolColor::ReadOnly);
}

#[test]
fn qwen_grep_alias_uses_canonical_display() {
    for name in ["grep", "grep_search"] {
        let info = display_for_tool(name, r#"{"pattern":"TODO","path":"src/"}"#);
        assert_eq!(info.label, "Grep");
        assert_eq!(info.preview, "/TODO/ in src/");
        assert_eq!(info.color, ToolColor::ReadOnly);
    }
}

#[test]
fn file_search_query_alias_uses_query_as_search_target() {
    for name in ["FileSearch", "file_search", "search_file_content"] {
        let presentation = presentation_for_tool(name, r#"{"query":"needle","path":"src/"}"#);
        assert_eq!(
            presentation.kind,
            ToolPresentationKind::FileSearch,
            "{name}"
        );
        assert_eq!(presentation.canonical_name, "Grep", "{name}");
        assert_eq!(presentation.target.as_deref(), Some("\"needle\" in src/"));
        assert_eq!(presentation.preview, "/needle/ in src/");
    }
}

#[test]
fn glob_tool() {
    let info = display_for_tool("Glob", r#"{"pattern":"**/*.rs"}"#);
    assert_eq!(info.label, "Glob");
    assert_eq!(info.preview, "**/*.rs");
    assert_eq!(info.color, ToolColor::ReadOnly);
}

#[test]
fn qwen_ls_alias_uses_canonical_display() {
    let info = display_for_tool("list_directory", r#"{"path":"src"}"#);
    assert_eq!(info.label, "LS");
    assert_eq!(info.preview, "src");
    assert_eq!(info.color, ToolColor::ReadOnly);
}

#[test]
fn lsp_tool() {
    let info = display_for_tool(
        "LSP",
        r#"{"operation":"goToDefinition","filePath":"src/main.rs","line":42}"#,
    );
    assert_eq!(info.label, "LSP goToDefinition");
    assert_eq!(info.preview, "src/main.rs:42");
    assert_eq!(info.color, ToolColor::ReadOnly);
}

#[test]
fn web_fetch_tool() {
    let info = display_for_tool("WebFetch", r#"{"url":"https://example.com"}"#);
    assert_eq!(info.label, "WebFetch");
    assert_eq!(info.preview, "https://example.com");
    assert_eq!(info.color, ToolColor::ReadOnly);
}

#[test]
fn web_search_tool() {
    let info = display_for_tool("WebSearch", r#"{"query":"rust async"}"#);
    assert_eq!(info.label, "WebSearch");
    assert_eq!(info.preview, "\"rust async\"");
    assert_eq!(info.color, ToolColor::ReadOnly);
}

#[test]
fn agent_aliases_use_bounded_prompt_task_or_description_target() {
    let long_prompt = format!(r#"{{"prompt":"{}"}}"#, "review ".repeat(40));
    let cases = [
        ("Task", r#"{"task":"Review the tool card result quality"}"#),
        ("Subagent", r#"{"description":"Audit display.rs"}"#),
        ("Delegate", long_prompt.as_str()),
    ];

    for (name, input) in cases {
        let presentation = presentation_for_tool(name, input);
        assert_eq!(presentation.kind, ToolPresentationKind::Agent, "{name}");
        assert_eq!(presentation.canonical_name, name, "{name}");
        assert!(presentation
            .target
            .as_deref()
            .is_some_and(|target| target.len() <= 83));
        assert!(!presentation.preview.contains("\"prompt\""), "{name}");
    }
}

#[test]
fn context_mutation_tools_use_specific_receipt_canonical_names() {
    let cases = [
        ("save_memory", r#"{"fact":"keep this"}"#, "Memory", "saved"),
        ("TodoWrite", r#"{"task_id":"todo-1"}"#, "Todo", "updated"),
        ("TaskCreate", r#"{"title":"follow up"}"#, "Task", "created"),
        ("CronDelete", r#"{"cron_id":"cron-1"}"#, "Cron", "deleted"),
        (
            "ScheduleWakeup",
            r#"{"time":"2026-06-27T09:00:00+08:00"}"#,
            "Wakeup",
            "scheduled",
        ),
    ];

    for (name, input, canonical, receipt) in cases {
        let presentation = presentation_for_tool(name, input);
        assert_eq!(presentation.kind, ToolPresentationKind::Memory, "{name}");
        assert_eq!(presentation.canonical_name, canonical, "{name}");
        assert_eq!(presentation.secondary.as_deref(), Some(receipt), "{name}");
    }
}

#[test]
fn tool_presentation_covers_spec_tool_aliases() {
    let cases = [
        (
            "ReadFolder",
            r#"{"path":"src"}"#,
            ToolPresentationKind::DirectoryList,
            ToolImpact::ReadOnly,
            "LS",
        ),
        (
            "search_file_content",
            r#"{"pattern":"TODO","path":"src"}"#,
            ToolPresentationKind::FileSearch,
            ToolImpact::ReadOnly,
            "Grep",
        ),
        (
            "NotebookEdit",
            r#"{"file_path":"notes.ipynb","old_string":"a","new_string":"b"}"#,
            ToolPresentationKind::FileEdit,
            ToolImpact::Write,
            "Notebook edit",
        ),
        (
            "edit",
            r#"{"file_path":"notes.md","old_string":"a","new_string":"b"}"#,
            ToolPresentationKind::FileEdit,
            ToolImpact::Write,
            "Edit",
        ),
        (
            "web_fetch",
            r#"{"url":"https://example.com"}"#,
            ToolPresentationKind::WebFetch,
            ToolImpact::OpenWorld,
            "WebFetch",
        ),
        (
            "google_web_search",
            r#"{"query":"rust async"}"#,
            ToolPresentationKind::WebSearch,
            ToolImpact::OpenWorld,
            "WebSearch",
        ),
        (
            "read_skill",
            r#"{"skill":"linux_memory"}"#,
            ToolPresentationKind::Skill,
            ToolImpact::ContextMutation,
            "Skill",
        ),
        (
            "skill",
            r#"{"action":"list"}"#,
            ToolPresentationKind::Skill,
            ToolImpact::ContextMutation,
            "Skill",
        ),
        (
            "TodoWrite",
            r#"{"task_id":"task-1"}"#,
            ToolPresentationKind::Memory,
            ToolImpact::ContextMutation,
            "Todo",
        ),
        (
            "todo",
            r#"{"task_id":"task-1"}"#,
            ToolPresentationKind::Memory,
            ToolImpact::ContextMutation,
            "Todo",
        ),
        (
            "Agent",
            r#"{"name":"reviewer"}"#,
            ToolPresentationKind::Agent,
            ToolImpact::ContextMutation,
            "Agent",
        ),
        (
            "AskUserQuestion",
            r#"{"question":"Pick one"}"#,
            ToolPresentationKind::Question,
            ToolImpact::Unknown,
            "Question",
        ),
        (
            "cosh_shell_evidence",
            r#"{"action":"read_output","output_id":"terminal-output://s/c"}"#,
            ToolPresentationKind::ShellEvidence,
            ToolImpact::ReadOnly,
            "Evidence",
        ),
    ];

    for (name, input, kind, impact, canonical) in cases {
        let presentation = presentation_for_tool(name, input);
        assert_eq!(presentation.kind, kind, "{name}");
        assert_eq!(presentation.impact, impact, "{name}");
        assert_eq!(presentation.canonical_name, canonical, "{name}");
        assert!(!presentation.preview.contains("\"content\""), "{name}");
        assert!(
            !presentation.preview.contains("terminal-output://"),
            "{name}"
        );
    }
}

#[test]
fn tool_presentation_covers_every_kind() {
    let cases = [
        (
            "Bash",
            r#"{"command":"pwd"}"#,
            ToolPresentationKind::ShellCommand,
        ),
        (
            "Read",
            r#"{"file_path":"Cargo.toml"}"#,
            ToolPresentationKind::FileRead,
        ),
        (
            "Write",
            r#"{"file_path":"out.txt","content":"ok"}"#,
            ToolPresentationKind::FileWrite,
        ),
        (
            "Edit",
            r#"{"file_path":"out.txt","old_string":"a","new_string":"b"}"#,
            ToolPresentationKind::FileEdit,
        ),
        (
            "Grep",
            r#"{"pattern":"TODO","path":"src"}"#,
            ToolPresentationKind::FileSearch,
        ),
        (
            "Glob",
            r#"{"pattern":"**/*.rs"}"#,
            ToolPresentationKind::FileGlob,
        ),
        (
            "LS",
            r#"{"path":"src"}"#,
            ToolPresentationKind::DirectoryList,
        ),
        (
            "read_many_files",
            r#"{"paths":["a.rs","b.rs"]}"#,
            ToolPresentationKind::MultiFileRead,
        ),
        (
            "LSP",
            r#"{"operation":"hover","filePath":"src/main.rs","line":1}"#,
            ToolPresentationKind::Lsp,
        ),
        (
            "WebFetch",
            r#"{"url":"https://example.com"}"#,
            ToolPresentationKind::WebFetch,
        ),
        (
            "WebSearch",
            r#"{"query":"rust"}"#,
            ToolPresentationKind::WebSearch,
        ),
        (
            "Skill",
            r#"{"skill":"linux_memory"}"#,
            ToolPresentationKind::Skill,
        ),
        ("skill", r#"{"action":"list"}"#, ToolPresentationKind::Skill),
        (
            "Agent",
            r#"{"name":"reviewer"}"#,
            ToolPresentationKind::Agent,
        ),
        (
            "save_memory",
            r#"{"fact":"remember"}"#,
            ToolPresentationKind::Memory,
        ),
        (
            "AskUserQuestion",
            r#"{"question":"Pick one"}"#,
            ToolPresentationKind::Question,
        ),
        (
            "cosh_shell_evidence",
            r#"{"action":"list_commands"}"#,
            ToolPresentationKind::ShellEvidence,
        ),
        (
            "CustomTool",
            r#"{"name":"custom"}"#,
            ToolPresentationKind::Custom,
        ),
    ];

    for (name, input, kind) in cases {
        assert_eq!(presentation_for_tool(name, input).kind, kind, "{name}");
    }
}

#[test]
fn terminal_output_read_target_uses_bookmark_label() {
    let presentation = presentation_for_tool(
        "Read",
        r#"{"file_path":"terminal-output://session-1/cmd-1"}"#,
    );

    assert_eq!(presentation.kind, ToolPresentationKind::FileRead);
    assert_eq!(
        presentation.target.as_deref(),
        Some("Shell output bookmark")
    );
    assert_eq!(presentation.preview, "terminal-output://session-1/cmd-1");
}

#[test]
fn unknown_tool() {
    let info = display_for_tool("CustomTool", r#"{"x":1}"#);
    assert_eq!(info.label, "CustomTool");
    assert_eq!(info.color, ToolColor::Unknown);
    assert_eq!(info.preview, "input: structured payload");
}

#[test]
fn malformed_json_fallback() {
    let info = display_for_tool("Bash", "not json at all");
    assert_eq!(info.label, "Bash");
    assert_eq!(info.preview, "$ not json at all");
    assert_eq!(info.color, ToolColor::Execute);
}

#[test]
fn malformed_json_unknown_tool() {
    let info = display_for_tool("Foo", "broken{json");
    assert_eq!(info.label, "Foo");
    assert_eq!(info.color, ToolColor::Unknown);
    assert_eq!(info.preview, "input: opaque payload");
}

#[test]
fn unknown_mcp_tool_extracts_server_and_tool_without_raw_json() {
    let presentation = presentation_for_tool(
        "mcp__github__create_issue",
        r#"{"title":"Bug","body":"secret body"}"#,
    );
    assert_eq!(presentation.kind, ToolPresentationKind::Custom);
    assert_eq!(
        presentation.preview,
        "server: github; tool: create_issue; title: Bug"
    );
    let raw = presentation
        .raw_input_preview
        .as_deref()
        .unwrap_or_default();
    assert!(raw.contains(r#""title":"Bug""#));
    assert!(raw.contains(r#""body":"secret body""#));
}

#[test]
fn read_many_files_caps_visible_paths_at_twenty() {
    let paths = (0..25)
        .map(|idx| format!(r#""file-{idx:02}.rs""#))
        .collect::<Vec<_>>()
        .join(",");
    let input = format!(r#"{{"paths":[{paths}]}}"#);

    let presentation = presentation_for_tool("read_many_files", &input);

    assert_eq!(presentation.kind, ToolPresentationKind::MultiFileRead);
    assert_eq!(presentation.target.as_deref(), Some("25 files"));
    assert!(presentation.preview.contains("file-00.rs"));
    assert!(presentation.preview.contains("file-19.rs"));
    assert!(presentation.preview.contains("+5 more"));
    assert!(!presentation.preview.contains("file-20.rs"));
    assert_eq!(
        presentation
            .fields
            .iter()
            .filter(|field| field.label == "path")
            .count(),
        20
    );
    assert!(presentation
        .fields
        .iter()
        .any(|field| { field.label == "omitted_paths" && field.value == "5" }));
    assert!(!presentation
        .fields
        .iter()
        .any(|field| field.value == "file-20.rs"));
}

#[test]
fn malformed_unknown_tool_is_opaque_but_auditable() {
    let presentation = presentation_for_tool("CustomTool", "not-json-secret");
    assert_eq!(presentation.preview, "input: opaque payload");
    assert_eq!(
        presentation.raw_input_preview.as_deref(),
        Some("not-json-secret")
    );
}

#[test]
fn truncate_preserves_utf8_boundaries() {
    let preview = truncate("内存占用分析建议".repeat(20).as_str(), 120);
    assert!(preview.ends_with("..."));
    assert!(preview.is_char_boundary(preview.len()));
}

#[test]
fn compact_json_preserves_utf8_boundaries() {
    let json = serde_json::json!({
        "query": "内存占用分析建议".repeat(20)
    })
    .to_string();
    let preview = compact_json(&json);
    assert!(preview.ends_with("..."));
    assert!(preview.is_char_boundary(preview.len()));
}
