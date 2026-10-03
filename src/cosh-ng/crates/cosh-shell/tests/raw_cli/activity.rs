use super::*;

#[test]
fn raw_cli_details_for_activity_uses_structured_panel() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? request tool approval\n"),
            ("stdout: 24", b"/details out-1\n"),
            ("Activity details out-1", b"exit\n"),
        ],
    );

    assert!(output.contains("Activity details out-1"), "{output}");
    assert!(output.contains("Tool output - stdout captured"), "{output}");
    assert!(output.contains("Run: agent-request-input-"), "{output}");
    assert!(output.contains("Detail:"), "{output}");
    assert!(output.contains("tool: tool-1"), "{output}");
    assert!(output.contains("stream: stdout"), "{output}");
    assert!(output.contains("line 24: fake tool output"), "{output}");
    assert!(!output.contains("Skill loaded: git-project"), "{output}");
    assert!(
        !output.contains("Tool output: stdout captured; [Details] out-1"),
        "{output}"
    );
    assert!(!output.contains("Tool completed"), "{output}");
    assert!(!output.contains("skill-2 skill:"), "{output}");
    assert!(!output.contains("out-1 output:"), "{output}");
    assert!(!output.contains("tool-1 tool:"), "{output}");
    assert!(!output.contains("id: out-1"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_activity_details_uses_zh_language_env() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? request tool approval\n"),
            ("stdout: 24", b"/details out-1\n"),
            ("活动详情 out-1", b"exit\n"),
        ],
    );

    assert!(output.contains("活动详情 out-1"), "{output}");
    assert!(output.contains("Tool 输出 - stdout 已捕获"), "{output}");
    assert!(output.contains("运行: agent-request-input-"), "{output}");
    assert!(output.contains("详情:"), "{output}");
    assert!(output.contains("tool: tool-1"), "{output}");
    assert!(output.contains("stream: stdout"), "{output}");
    assert!(output.contains("line 24: fake tool output"), "{output}");
    assert!(!output.contains("Activity details out-1"), "{output}");
    assert!(!output.contains("output - stdout captured"), "{output}");
    assert!(!output.contains("Run: agent-request-input-"), "{output}");
    assert!(!output.contains("Detail:"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
    assert_no_migrated_english_ui_labels(&output, DETAILS_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_missing_details_uses_structured_notice_and_keeps_shell_usable() {
    let output = run_raw_cli_with_input(
        "fake",
        "/details missing-id\n\
         echo after-missing-details\n\
         exit\n",
    );

    assert!(output.contains("Details unavailable"), "{output}");
    assert!(
        output.contains(
            "missing-id is not available; use a Details action with an approval or activity id"
        ),
        "{output}"
    );
    assert!(output.contains("after-missing-details"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_missing_details_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "/details missing-id\n\
         echo after-missing-details\n\
         exit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("详情不可用"), "{output}");
    assert!(
        output.contains("missing-id 不可用；请对审批或活动 id 使用 Details 操作"),
        "{output}"
    );
    assert!(!output.contains("Details unavailable"), "{output}");
    assert!(output.contains("after-missing-details"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
    assert_no_migrated_english_ui_labels(&output, DETAILS_ZH_FORBIDDEN_UI);
}
