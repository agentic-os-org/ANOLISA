use super::*;

fn run_renderer_case(input: &'static [u8], envs: &[(&str, &str)]) -> String {
    run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        envs,
        vec![
            (input.to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    )
}

#[test]
fn raw_cli_tool_output_does_not_break_markdown_stream_finalization() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? tool output finalization\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );

    assert!(output.contains("Before tool"), "{output}");
    assert!(output.contains("After tool"), "{output}");
    assert_ordered(&output, &["Before tool", "After tool"]);
    assert!(!output.contains("Tool output:"), "{output}");
    assert!(!output.contains("Tool completed"), "{output}");
    assert!(!output.contains("Governance:"), "{output}");
    assert!(
        !output.contains("bash: ?? tool output finalization"),
        "{output}"
    );
}

#[test]
fn raw_cli_no_color_keeps_box_layout_when_terminal_supports_it() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("NO_COLOR", "1"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (b"?? recommendation fixture\n".to_vec(), Duration::ZERO),
            (b"exit 0\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("╭─ Recommendations"), "{output}");
    assert!(!output.contains("╭─ Agent status"));
}

#[test]
fn raw_cli_agent_response_renders_markdown_inside_card() {
    let output = run_renderer_case(
        b"?? render markdown\n",
        &[("COSH_SHELL_LANG", "en-US"), ("TERM", "xterm-256color")],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ Project check"), "{output}");
    assert!(output.contains("│ ─────────────────"), "{output}");
    assert!(!output.contains("│ ============="), "{output}");
    assert!(output.contains("│ • Run git status"), "{output}");
    assert!(output.contains("│ • Build workspace"), "{output}");
    assert!(
        output.contains("│   ◦ Use package scoped tests"),
        "{output}"
    );
    assert!(
        output.contains("│   1. Keep shell-first validation repeatable"),
        "{output}"
    );
    assert!(
        output.contains("│ 1. Review rendered transcript"),
        "{output}"
    );
    assert!(output.contains("│ ┌ code: bash"), "{output}");
    assert!(output.contains("│ │ cargo build --workspace"), "{output}");
    assert!(output.contains("│ │ if test -d crates; then"), "{output}");
    assert!(
        output.contains("│ │   cargo test --package cosh-shell"),
        "{output}"
    );
    assert!(output.contains("│ │ fi"), "{output}");
    assert!(
        output.contains("│ │ Commands are suggestions only."),
        "{output}"
    );
    assert!(
        !output.contains("│ > Commands are suggestions only."),
        "{output}"
    );
    assert!(!output.contains("```bash"), "{output}");
    assert!(!output.contains("```"), "{output}");
}

#[test]
fn raw_cli_zh_agent_response_renders_markdown_labels() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN"), ("TERM", "xterm-256color")],
        vec![
            (b"?? render markdown\n".to_vec(), Duration::ZERO),
            (
                b"?? render markdown table\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_000)),
        ],
    );

    assert!(output.contains("╭ Agent 回复"), "{output}");
    assert!(output.contains("│ ┌ 代码: bash"), "{output}");
    assert!(output.contains("│ ┌ 表格"), "{output}");
    assert!(output.contains("│ Project check"), "{output}");
    assert!(output.contains("│ 内存占用 Top 10 分析:"), "{output}");
    assert!(!output.contains("╭ Agent ─"), "{output}");
    assert!(!output.contains("│ ┌ code: bash"), "{output}");
    assert!(!output.contains("│ ┌ table"), "{output}");
    assert_no_migrated_english_ui_labels(&output, RENDERER_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_agent_response_streams_markdown_fragments_inside_card() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US"), ("TERM", "xterm-256color")],
        vec![
            (b"?? stream markdown\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(800)),
        ],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ Streaming check"), "{output}");
    assert!(output.contains("│ ─────────────────"), "{output}");
    assert!(!output.contains("│ ==============="), "{output}");
    assert!(output.contains("│ • First item"), "{output}");
    assert!(output.contains("│ • Second item"), "{output}");
    assert!(
        output.contains("│ │ cargo test --package cosh-shell"),
        "{output}"
    );
    assert!(output.contains("│ Done."), "{output}");
    assert!(!output.contains("# Streaming check"), "{output}");
    assert!(!output.contains("```bash"), "{output}");
}

#[test]
fn raw_cli_agent_response_streams_markdown_table_as_stable_block() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US"), ("TERM", "xterm-256color")],
        vec![
            (b"?? stream markdown table\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(800)),
        ],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ Streaming table"), "{output}");
    assert!(output.contains("│ ─────────────────"), "{output}");
    assert!(!output.contains("│ ==============="), "{output}");
    assert!(output.contains("│ ┌ table"), "{output}");
    assert!(output.contains("│ │排名"), "{output}");
    assert!(output.contains("ps aux | grep cosh"), "{output}");
    assert!(output.contains("│ Done."), "{output}");
    assert!(!output.contains("# Streaming table"), "{output}");
    assert!(!output.contains("| --- | --- | --- |"), "{output}");
}

#[test]
fn raw_cli_agent_response_streams_soft_wrapped_markdown_paragraph() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US"), ("TERM", "xterm-256color")],
        vec![
            (b"?? stream markdown paragraph\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(800)),
        ],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ Streaming paragraph"), "{output}");
    assert!(
        output.contains("This Agent answer starts and continues"),
        "{output}"
    );
    assert!(output.contains("source line with 中文内容."), "{output}");
    assert!(
        !output.contains("starts\n│ and continues on another source line"),
        "{output}"
    );
    assert!(output.contains("│ Done."), "{output}");
}

#[test]
fn raw_cli_agent_response_renders_markdown_table_inside_card() {
    let output = run_renderer_case(
        b"?? render markdown table\n",
        &[("COSH_SHELL_LANG", "en-US"), ("TERM", "xterm-256color")],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ 内存占用 Top 10 分析:"), "{output}");
    assert!(output.contains("│ ┌ table"), "{output}");
    assert!(output.contains("│ │排名"), "{output}");
    assert!(output.contains("│ │1"), "{output}");
    assert!(output.contains("Virtualizatio"), "{output}");
    assert!(
        output.contains("n.VirtualMach") || output.contains("n.VirtualMachine"),
        "{output}"
    );
    assert!(output.contains("ps aux | grep cosh"), "{output}");
    assert!(output.contains("│ 关键发现：Qoder 占用最多。"), "{output}");
    assert!(!output.contains("| --- | --- | --- | --- |"), "{output}");
}

#[test]
fn raw_cli_agent_response_renders_markdown_table_at_configured_narrow_width() {
    let output = run_renderer_case(
        b"?? render markdown table\n",
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_WIDTH", "54"),
        ],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ 内存占用 Top 10 分析:"), "{output}");
    assert!(output.contains("│ ┌ table"), "{output}");
    assert!(output.contains("Virtualizatio"), "{output}");
    assert!(output.contains("VirtualMachine"), "{output}");
    assert!(output.contains("ps aux | grep c"), "{output}");
    assert!(output.contains("osh"), "{output}");
    assert!(output.contains("│ 关键发现：Qoder 占用最多。"), "{output}");
    assert!(!output.contains("| --- | --- | --- | --- |"), "{output}");
    assert_agent_block_width(&output, 54);
}

#[test]
fn raw_cli_agent_response_keeps_markdown_pipe_output_without_separator_as_text() {
    let output = run_renderer_case(
        b"?? render markdown pipe output\n",
        &[("COSH_SHELL_LANG", "en-US"), ("TERM", "xterm-256color")],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ Shell output:"), "{output}");
    assert!(
        output.contains("│ | 1 | Virtualization.VirtualMachine | ~1470 MB |"),
        "{output}"
    );
    assert!(output.contains("│ | 2 | Node | ~572 MB |"), "{output}");
    assert!(output.contains("│ Done."), "{output}");
    assert!(!output.contains("│ ┌ table"), "{output}");
}

#[test]
fn raw_cli_agent_response_renders_indented_markdown_code_inside_card() {
    let output = run_renderer_case(
        b"?? render markdown indented code\n",
        &[("COSH_SHELL_LANG", "en-US"), ("TERM", "xterm-256color")],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ Indented code check"), "{output}");
    assert!(output.contains("│ ┌ code "), "{output}");
    assert!(
        output.contains("│ │ cargo test --package cosh-shell"),
        "{output}"
    );
    assert!(output.contains("│ │ git status --short"), "{output}");
    assert!(output.contains("│ Done."), "{output}");
    assert!(!output.contains("│     cargo test"), "{output}");
    assert!(!output.contains("```"), "{output}");
}

#[test]
fn raw_cli_agent_response_joins_soft_wrapped_markdown_paragraph() {
    let output = run_renderer_case(
        b"?? render markdown paragraph\n",
        &[("COSH_SHELL_LANG", "en-US"), ("TERM", "xterm-256color")],
    );

    assert!(output.contains("╭ Agent"), "{output}");
    assert!(output.contains("│ Paragraph rendering"), "{output}");
    assert!(
        output.contains("This Agent answer is split across"),
        "{output}"
    );
    assert!(output.contains("source lines with 中文内容"), "{output}");
    assert!(output.contains("as one"), "{output}");
    assert!(output.contains("Markdown paragraph."), "{output}");
    assert!(
        !output.contains("split\n│ across multiple source lines"),
        "{output}"
    );
}

#[test]
fn raw_cli_agent_response_renders_markdown_in_plain_mode() {
    let output = run_renderer_case(
        b"?? render markdown\n",
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_RENDER", "plain"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("Agent:"), "{output}");
    assert!(output.contains("  Project check"), "{output}");
    assert!(output.contains("  ============="), "{output}");
    assert!(output.contains("  - Run git status"), "{output}");
    assert!(
        output.contains("    1. Keep shell-first validation repeatable"),
        "{output}"
    );
    assert!(
        output.contains("  1. Review rendered transcript"),
        "{output}"
    );
    assert!(output.contains("  +-- code: bash"), "{output}");
    assert!(output.contains("  | cargo build --workspace"), "{output}");
    assert!(output.contains("  | if test -d crates; then"), "{output}");
    assert!(
        output.contains("  |   cargo test --package cosh-shell"),
        "{output}"
    );
    assert!(output.contains("  | fi"), "{output}");
    assert!(!output.contains("# Project check"), "{output}");
    assert!(!output.contains("```bash"), "{output}");
}

#[test]
fn raw_cli_dumb_terminal_uses_plain_blocks() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("NO_COLOR", "1"),
            ("TERM", "dumb"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (b"?? recommendation fixture\n".to_vec(), Duration::ZERO),
            (b"exit 0\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );

    assert_plain_blocks(&output);
}

#[test]
fn raw_cli_explicit_plain_render_mode_uses_plain_blocks() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_RENDER", "plain"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (b"?? recommendation fixture\n".to_vec(), Duration::ZERO),
            (b"exit 0\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );

    assert_plain_blocks(&output);
}

fn assert_plain_blocks(output: &str) {
    assert!(output.contains("Agent:"), "{output}");
    assert!(output.contains("Recommendations:"), "{output}");
    assert!(!output.contains("Agent status:"));
    assert!(!output.contains('╭'));
    assert!(!output.contains('│'));
    assert!(!output.contains('╰'));
}
