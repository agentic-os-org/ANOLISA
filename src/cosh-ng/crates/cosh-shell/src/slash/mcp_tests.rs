use crate::{I18n, Language};
fn format_mcp_json(json: &serde_json::Value) -> Vec<String> {
    super::format_mcp_json(json, &I18n::new(Language::EnUs))
}
fn format_mcp_output(stdout: &str) -> Vec<String> {
    super::format_mcp_output(stdout, &I18n::new(Language::EnUs))
}
fn mcp_success_body(stdout: &str, stderr: &str) -> Vec<String> {
    super::mcp_success_body(stdout, stderr, &I18n::new(Language::EnUs), &[])
}
fn extract_json_message_or_error(stdout: &str) -> Option<String> {
    super::extract_json_message_or_error(stdout, &I18n::new(Language::EnUs))
}

#[test]
fn review_regression_mcp_chinese_login_and_usage() {
    let adapter = crate::adapter::AdapterInstance::Fake(crate::adapter::FakeAgentAdapter);
    let mut state = crate::runtime::prelude::InlineState {
        language: crate::Language::ZhCn,
        ..Default::default()
    };
    for (action, expected) in [
        ("login", "浏览器"),
        ("help", "用法"),
        ("connect", "服务器"),
        ("unknown", "未知"),
    ] {
        let mut output = Vec::new();
        super::render_mcp_command(Some(action), None, None, &adapter, &mut state, &mut output)
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(expected), "{action}: {output}");
        if action == "login" {
            assert!(output.contains("cosh-core mcp login <server>"), "{output}");
        }
    }
}

#[cfg(unix)]
fn render_success_fixture(action: &str, body: &str, language: Language) -> String {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("core");
    std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = crate::adapter::AdapterInstance::CoshCore(crate::adapter::CoshCoreAdapter::new(
        program.to_string_lossy().into_owned(),
        false,
    ));
    let mut state = crate::runtime::prelude::InlineState {
        language,
        ..Default::default()
    };
    let mut output = Vec::new();
    super::render_mcp_command(
        Some(action),
        Some("remote"),
        None,
        &adapter,
        &mut state,
        &mut output,
    )
    .unwrap();
    String::from_utf8(output).unwrap()
}

#[cfg(unix)]
fn assert_inspection_success(action: &str, response: &str, chinese: &str) {
    let body = format!("printf '%s\\n' '{{\"server\":\"remote\",\"action\":\"{response}\",\"transport\":\"stdio\",\"tools\":[{{\"name\":\"EnglishTool\"}}]}}'");
    for (language, expected) in [(Language::EnUs, response), (Language::ZhCn, chinese)] {
        let output = render_success_fixture(action, &body, language);
        assert!(output.contains(expected), "{output}");
        assert!(output.contains("remote"), "{output}");
        assert!(output.contains("EnglishTool"), "{output}");
    }
}

#[cfg(unix)]
#[test]
fn review_regression_mcp_success_connected() {
    assert_inspection_success("connect", "connected", "已连接");
}

#[cfg(unix)]
#[test]
fn review_regression_mcp_success_inspected() {
    assert_inspection_success("inspect", "inspected", "已查看工具");
}

#[cfg(unix)]
#[test]
fn review_regression_mcp_success_refreshed() {
    assert_inspection_success("refresh", "refreshed", "已刷新工具");
}

#[cfg(unix)]
#[test]
fn review_regression_mcp_success_disconnected() {
    for removed in [true, false] {
        let body = format!("printf '%s\\n' '{{\"server\":\"remote\",\"disabled\":true,\"credentials_removed\":{removed}}}'");
        for (language, expected, credentials) in [
            (
                Language::EnUs,
                "disconnected",
                if removed { "removed" } else { "No saved" },
            ),
            (
                Language::ZhCn,
                "已断开",
                if removed { "已删除" } else { "无已保存" },
            ),
        ] {
            let output = render_success_fixture("disconnect", &body, language);
            assert!(output.contains(expected), "{output}");
            assert!(output.contains(credentials), "{output}");
            assert!(output.contains("remote"), "{output}");
            assert!(!output.contains("credentials_removed"), "{output}");
        }
    }
}

#[cfg(unix)]
#[test]
fn review_regression_mcp_success_logout_keeps_additional_stderr() {
    let body = "printf \"OAuth credentials removed for MCP server 'remote'.\\nThird-party notice: preserve me\\n\" >&2";
    for (language, expected) in [
        (Language::EnUs, "OAuth credentials removed"),
        (Language::ZhCn, "已删除"),
    ] {
        let output = render_success_fixture("logout", body, language);
        assert!(output.contains(expected), "{output}");
        assert!(output.contains("remote"), "{output}");
        assert!(
            output.contains("Third-party notice: preserve me"),
            "{output}"
        );
        if language == Language::ZhCn {
            assert!(!output.contains("OAuth credentials removed"), "{output}");
        }
    }
}

#[cfg(unix)]
#[test]
fn review_regression_mcp_chinese_server_status_preserves_protocol_names() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("core");
    std::fs::write(&program, "#!/bin/sh\nprintf '%s\\n' '{\"servers\":[{\"server\":\"remote\",\"status\":\"connected\",\"transport\":\"streamable-http\",\"tools\":[{\"name\":\"EnglishTool\"}]}]}'\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = crate::adapter::AdapterInstance::CoshCore(crate::adapter::CoshCoreAdapter::new(
        program.to_string_lossy().into_owned(),
        false,
    ));
    let mut state = crate::runtime::prelude::InlineState {
        language: crate::Language::ZhCn,
        ..Default::default()
    };
    let mut output = Vec::new();
    super::render_mcp_command(Some("list"), None, None, &adapter, &mut state, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("已连接"), "{output}");
    assert!(output.contains("工具"), "{output}");
    assert!(output.contains("streamable-http"), "{output}");
    assert!(output.contains("EnglishTool"), "{output}");
}

#[test]
fn success_panel_falls_back_to_stderr_when_stdout_is_empty() {
    let confirmation = "OAuth credentials removed for MCP server 'remote'.";
    assert_eq!(
        mcp_success_body("", &format!("{confirmation}\n")),
        vec![confirmation.to_string()]
    );
    assert_eq!(
        mcp_success_body("{\"message\":\"connected\"}", "progress noise"),
        vec!["connected".to_string()]
    );
    assert!(mcp_success_body("", "").is_empty());
}

#[test]
fn server_status_falls_back_to_enabled_then_unknown() {
    let json = serde_json::json!({"servers": [
        {"server": "a", "status": "pending"},
        {"server": "b", "enabled": true},
        {"server": "c", "enabled": false},
        {"server": "d"},
    ]});
    let lines = format_mcp_json(&json);
    assert!(
        lines[0].contains("a") && lines[0].contains("[pending]"),
        "{lines:?}"
    );
    assert!(
        lines[1].contains("b") && lines[1].contains("[enabled]"),
        "{lines:?}"
    );
    assert!(
        lines[2].contains("c") && lines[2].contains("[disabled]"),
        "{lines:?}"
    );
    assert!(
        lines[3].contains("d") && lines[3].contains("[unknown]"),
        "{lines:?}"
    );
}

#[test]
fn action_tool_list_truncation_matches_servers_branch() {
    let tools: Vec<serde_json::Value> = (0..23)
        .map(|index| serde_json::json!({"name": format!("tool-{index}")}))
        .collect();
    let action = serde_json::json!({"action": "inspect", "tools": tools});
    let action_lines = format_mcp_json(&action);
    assert!(
        action_lines.contains(&"  ... and 3 more".to_string()),
        "{action_lines:?}"
    );

    let servers = serde_json::json!({"servers": [
        {"server": "a", "status": "enabled", "tools": tools},
    ]});
    let server_lines = format_mcp_json(&servers);
    assert!(
        server_lines.contains(&"      ... and 3 more".to_string()),
        "{server_lines:?}"
    );
}

#[test]
fn message_and_error_extraction_share_one_path() {
    let message = serde_json::json!({"message": "connected"});
    assert_eq!(format_mcp_json(&message), vec!["connected".to_string()]);

    let error_string = serde_json::json!({"error": "not found"});
    assert_eq!(
        format_mcp_json(&error_string),
        vec!["Error: not found".to_string()]
    );

    let error_object = serde_json::json!({"error": {"message": "denied"}});
    assert_eq!(
        format_mcp_json(&error_object),
        vec!["Error: denied".to_string()]
    );

    assert_eq!(
        extract_json_message_or_error(r#"{"error": {"message": "denied"}}"#),
        Some("Error: denied".to_string())
    );
    assert_eq!(extract_json_message_or_error("not json"), None);
}

#[test]
fn non_json_output_falls_back_to_raw_lines() {
    assert_eq!(
        format_mcp_output("first\n\nsecond\n"),
        vec!["first".to_string(), "second".to_string()]
    );
}

#[test]
fn transport_absent_leaves_no_trailing_whitespace() {
    let json = serde_json::json!({"servers": [{"server": "a", "enabled": true}]});
    let lines = format_mcp_json(&json);
    assert_eq!(lines[0], lines[0].trim_end(), "{lines:?}");
}

#[cfg(unix)]
#[test]
fn subprocess_timeout_kills_slow_cosh_core() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    let root = std::env::temp_dir().join(format!("mcp-timeout-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let slow = root.join("slow-cosh-core");
    std::fs::write(&slow, "#!/bin/sh\nsleep 5\n").unwrap();
    std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o700)).unwrap();

    let started = std::time::Instant::now();
    let result = super::capture_mcp_cli_with_timeout(
        slow.to_str().unwrap(),
        &["mcp", "list"],
        Duration::from_millis(50),
        &I18n::new(Language::EnUs),
    );
    let Err(message) = result else {
        panic!("slow cosh-core must time out");
    };
    assert!(message.contains("timed out"), "{message}");
    assert!(started.elapsed() < Duration::from_secs(2));

    assert!(super::capture_mcp_cli(
        "/definitely/missing/cosh-core",
        &[],
        &I18n::new(Language::EnUs)
    )
    .is_err());
    let _ = std::fs::remove_dir_all(root);
}
