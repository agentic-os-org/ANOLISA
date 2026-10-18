use super::{
    strip_ansi_escape, ActivityDetailsPanelModel, ActivityPanelModel, ActivityRowModel,
    ActivityToolRowModel, ApprovalDetailsPanelModel, ApprovalJournalEntryModel,
    ApprovalJournalPanelModel, ApprovalPanelAction, ApprovalPanelModel, ApprovalReceiptPanelModel,
    HealthBannerModel, NoticePanelModel, QuestionAnswerPanelModel, QuestionInputFeedback,
    QuestionPanelModel, RatatuiInlineRenderer, RecommendationActionPanelModel,
    RecommendationPanelModel, ToolInvocationCardModel, ToolInvocationDensity, ToolInvocationTone,
};
use crate::diagnostics::health::{
    HealthCollector, HealthFact, HealthFactCategory, HealthFactSource, HealthFactValue,
    HealthFinding, HealthFindingCategory, HealthMessageId, HealthScanReport, HealthSeverity,
    HealthTryItem, HealthTryKind, HealthUnavailableReason, UnavailableCollector,
};
use crate::tools::display::ToolPresentationKind;
use crate::types::{
    AgentEvent, GovernanceDecision, GovernancePolicyDecision, GovernedEvent, QuestionSelectionMode,
};
use ratatui::text::Span;
use std::collections::BTreeMap;

mod approval;
mod markdown;
mod question;
#[test]
fn wraps_long_agent_text_with_ratatui() {
    let event = GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::TextDelta {
            run_id: "run-1".to_string(),
            text:
                "hello 你好 this is a long response that should wrap inside a narrow shell viewport"
                    .to_string(),
        },
        reason: "display".to_string(),
        display_text:
            "hello 你好 this is a long response that should wrap inside a narrow shell viewport"
                .to_string(),
        auto_execute: false,
    };

    let lines = RatatuiInlineRenderer::with_width(40).governed_event_lines(&[event]);

    assert!(lines.len() > 1, "{lines:?}");
    assert!(lines[0].starts_with("hello"));
    assert!(lines.iter().all(|line| line.chars().count() <= 40));
}

#[test]
fn governed_events_use_zh_renderer_labels_without_translating_commands() {
    let event = GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::Recommendation {
            run_id: "run-1".to_string(),
            summary: "建议先检查磁盘。".to_string(),
            commands: vec!["df -h".to_string(), "du -sh .".to_string()],
            auto_execute: false,
        },
        reason: "display".to_string(),
        display_text: "unused fallback".to_string(),
        auto_execute: false,
    };
    let renderer = RatatuiInlineRenderer::with_width(80).with_language(crate::Language::ZhCn);

    let lines = renderer.governed_event_lines(std::slice::from_ref(&event));
    let text = lines.join("\n");
    assert!(text.contains("推荐命令:"), "{text}");
    assert!(text.contains("df -h"), "{text}");
    assert!(!text.contains("recommended commands:"), "{text}");

    let mut output = Vec::new();
    renderer
        .write_governed_events(&mut output, &[event])
        .unwrap();
    let block = String::from_utf8(output).unwrap();
    assert!(block.contains("治理"), "{block}");
    assert!(!block.contains("Governance"), "{block}");
}

#[test]
fn streaming_agent_strips_bold_markers_without_dropping_bullets() {
    let renderer = RatatuiInlineRenderer::with_width(60);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream
        .write_delta(&mut output, "\n\n**建议:**\n* keep this bullet")
        .unwrap();
    stream.finish(&mut output, None).unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains("│ \n│ \n"));
    assert!(text.contains("│ 建议:"));
    assert!(text.contains("│ * keep this bullet"));
    assert!(!text.contains("**"));
}

#[test]
fn streaming_agent_uses_zh_catalog_title() {
    let renderer = RatatuiInlineRenderer::with_width(60).with_language(crate::Language::ZhCn);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream.write_delta(&mut output, "你好").unwrap();
    stream.finish(&mut output, None).unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("╭ Agent 回复"), "{text}");
    assert!(!text.contains("╭ Agent ─"), "{text}");
}

#[test]
fn streaming_agent_suppresses_code_fence_language_and_left_trims_lines() {
    let renderer = RatatuiInlineRenderer::with_width(80);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream
        .write_delta(
            &mut output,
            "原因: 目标不存在。\n```bash\n    ls\n    find . -name \"ccc*\"\n```\n完成。",
        )
        .unwrap();
    stream.finish(&mut output, None).unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("│ 原因: 目标不存在。"));
    assert!(text.contains("│ ls"));
    assert!(text.contains("│ find . -name \"ccc*\""));
    assert!(text.contains("│ 完成。"));
    assert!(!text.contains("bash"));
    assert!(!text.contains("```"));
    assert!(!text.contains("│     ls"));
}

#[test]
fn streaming_agent_prefers_word_boundaries_across_deltas() {
    let renderer = RatatuiInlineRenderer::with_width(30);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream
        .write_delta(&mut output, "alpha beta gamma delta epsilon wo")
        .unwrap();
    stream.write_delta(&mut output, "rkspace command").unwrap();
    stream.finish(&mut output, None).unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains("epsilon wo\n│ rkspace"));
    assert!(text.contains("│ alpha beta gamma delta epsilon"), "{text}");
    assert!(
        text.contains("│ workspace command"),
        "streaming output should wrap before the whole word:\n{text}"
    );
}

#[test]
fn notice_panel_lines_matches_write_form_and_closes_borders() {
    // #2179: the offscreen form feeds the relay-injected hint card; it
    // must be byte-identical to the written panel so the hint card shares
    // the family framing, and every framed line must close both borders
    // at the panel standard width.
    let renderer = RatatuiInlineRenderer::with_width(80);
    let model = || NoticePanelModel {
        title: "Command is waiting for input",
        body: vec![
            "Type y or n:".to_string(),
            "Reply directly, or press Ctrl+C to interrupt.".to_string(),
        ],
        footer: None,
    };
    let lines = renderer.notice_panel_lines(model());
    let mut written = Vec::new();
    renderer.write_notice_panel(&mut written, model()).unwrap();
    assert_eq!(
        lines.join("\n") + "\n",
        String::from_utf8(written).unwrap(),
        "offscreen lines must match the written panel"
    );
    let joined = lines.join("\n");
    let width = usize::from(renderer.panel_standard_width());
    assert_box_lines_aligned(joined.trim_end(), width);
    assert!(lines.first().is_some_and(|l| l.trim_end().ends_with('╮')));
    assert!(lines.last().is_some_and(|l| l.trim_end().ends_with('╯')));
    for line in &lines[1..lines.len() - 1] {
        assert!(
            line.trim_end().ends_with('│'),
            "body rows must close the right border: {line:?}"
        );
    }

    // Plain form: same content contract, no box drawing.
    let plain = RatatuiInlineRenderer::plain_with_width(44).notice_panel_lines(model());
    assert!(plain[0].starts_with("· Command is waiting for input:"));
    assert!(plain.iter().all(|l| !l.contains('│')));
}

#[test]
fn plain_renderer_uses_text_blocks_without_box_drawing() {
    let renderer = RatatuiInlineRenderer::plain_with_width(44);
    let mut output = Vec::new();

    renderer
        .write_notice_panel(
            &mut output,
            NoticePanelModel {
                title: "Agent status",
                body: vec![
                    "Phase: requesting".to_string(),
                    "waiting for backend".to_string(),
                ],
                footer: None,
            },
        )
        .unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("Agent status:"));
    assert!(text.contains("  Phase: requesting"));
    assert!(!text.contains('╭'));
    assert!(!text.contains('│'));
    assert!(!text.contains('╰'));
}

#[test]
fn plain_streaming_agent_uses_text_prefix() {
    let renderer = RatatuiInlineRenderer::plain_with_width(44);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream
        .write_delta(&mut output, "hello from stream")
        .unwrap();
    stream
        .finish(
            &mut output,
            Some("Commands are suggestions only; nothing was executed."),
        )
        .unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("Agent:"));
    assert!(text.contains("  hello from stream"));
    assert!(text.contains("Commands are suggestions only"));
    assert!(text.contains("nothing"));
    assert!(text.contains("executed."));
    assert!(!text.contains('╭'));
    assert!(!text.contains('│'));
    assert!(!text.contains('╰'));
}

#[test]
fn activity_panel_renders_tool_output_rows() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![
                ActivityRowModel {
                    id: "out-1",
                    kind: "output",
                    status: "captured",
                    subject: "tool-1",
                    summary: "stdout captured; [Details] out-1",
                    tool: None,
                },
                ActivityRowModel {
                    id: "tool-1",
                    kind: "tool",
                    status: "completed",
                    subject: "tool-1",
                    summary: "exit 0",
                    tool: None,
                },
            ],
        })
        .join("\n");

    assert!(text.contains("Activity"), "{text}");
    assert!(
        text.contains("Tool output: stdout captured; [Details] out-1"),
        "{text}"
    );
    assert!(text.contains("Tool completed: exit 0"), "{text}");
    assert!(
        !text.contains("out-1 output: stdout captured; [Details] out-1"),
        "{text}"
    );
    assert!(!text.contains("tool-1 tool: completed"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn tool_invocation_card_renders_primary_result_and_metrics_without_details_link() {
    let renderer = RatatuiInlineRenderer::with_width(72);
    let text = renderer
        .tool_invocation_card_lines(ToolInvocationCardModel {
            title: "Read completed".to_string(),
            status: "success".to_string(),
            density: ToolInvocationDensity::Receipt,
            primary: "Cargo.toml".to_string(),
            result: "2 lines returned".to_string(),
            metrics: vec!["stdout: 2 lines".to_string()],
            action: None,
            debug_ref: None,
            tone: ToolInvocationTone::Success,
        })
        .join("\n");

    assert!(text.contains("Read completed"), "{text}");
    assert!(text.contains("Cargo.toml"), "{text}");
    assert!(text.contains("2 lines returned"), "{text}");
    assert!(text.contains("stdout: 2 lines"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert_rendered_width(&text, 72);
    assert_box_lines_aligned(&text, 72);
}

#[test]
fn tool_invocation_card_renders_debug_ref_without_details_link() {
    let renderer = RatatuiInlineRenderer::with_width(72);
    let text = renderer
        .tool_invocation_card_lines(ToolInvocationCardModel {
            title: "Read completed".to_string(),
            status: "success".to_string(),
            density: ToolInvocationDensity::Receipt,
            primary: "Cargo.toml".to_string(),
            result: "2 lines returned".to_string(),
            metrics: Vec::new(),
            action: None,
            debug_ref: Some("toolu-read".to_string()),
            tone: ToolInvocationTone::Success,
        })
        .join("\n");

    assert!(text.contains("debug: toolu-read"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert_rendered_width(&text, 72);
    assert_box_lines_aligned(&text, 72);
}

#[test]
fn plain_tool_invocation_card_keeps_same_semantic_content() {
    let renderer = RatatuiInlineRenderer::plain_with_width(40);
    let text = renderer
        .tool_invocation_card_lines(ToolInvocationCardModel {
            title: "Write requested".to_string(),
            status: "requested".to_string(),
            density: ToolInvocationDensity::ActionRequired,
            primary: "/tmp/cosh-write.txt".to_string(),
            result: "will modify workspace state".to_string(),
            metrics: Vec::new(),
            action: Some("approval required".to_string()),
            debug_ref: None,
            tone: ToolInvocationTone::Pending,
        })
        .join("\n");

    assert!(text.contains("Write requested"), "{text}");
    assert!(text.contains("/tmp/cosh-write.txt"), "{text}");
    assert!(text.contains("will modify workspace state"), "{text}");
    assert!(text.contains("approval required"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert!(!text.contains('╭'), "{text}");
}

#[test]
fn tool_invocation_card_narrow_width_keeps_primary_result_metrics_and_action() {
    let renderer = RatatuiInlineRenderer::with_width(40);
    let text = renderer
        .tool_invocation_card_lines(ToolInvocationCardModel {
            title: "Edit completed".to_string(),
            status: "success".to_string(),
            density: ToolInvocationDensity::Summary,
            primary: "src/main.rs".to_string(),
            result: "edit completed".to_string(),
            metrics: vec!["stdout: 1 line".to_string()],
            action: Some("review changes".to_string()),
            debug_ref: None,
            tone: ToolInvocationTone::Success,
        })
        .join("\n");

    assert!(text.contains("src/main.rs"), "{text}");
    assert!(text.contains("edit completed"), "{text}");
    assert!(text.contains("stdout: 1 line"), "{text}");
    assert!(text.contains("review changes"), "{text}");
    assert_rendered_width(&text, 40);
    assert_box_lines_aligned(&text, 40);
}

#[test]
fn tool_invocation_card_caps_long_primary_before_result_lines() {
    let renderer = RatatuiInlineRenderer::plain_with_width(32);
    let long_path = format!("/very/long/{}SENTINEL", "nested/".repeat(20));
    let lines = renderer.tool_invocation_card_lines(ToolInvocationCardModel {
        title: "Read completed".to_string(),
        status: "success".to_string(),
        density: ToolInvocationDensity::Receipt,
        primary: long_path,
        result: "RESULT SURVIVES".to_string(),
        metrics: vec!["stdout: 1 line".to_string()],
        action: Some("ACTION SURVIVES".to_string()),
        debug_ref: None,
        tone: ToolInvocationTone::Success,
    });
    let text = lines.join("\n");
    let result_index = lines
        .iter()
        .position(|line| line.contains("RESULT SURVIVES"))
        .expect(&text);

    assert!(result_index <= 3, "{text}");
    assert!(text.contains("stdout: 1 line"), "{text}");
    assert!(text.contains("ACTION SURVIVES"), "{text}");
    assert!(!text.contains("SENTINEL"), "{text}");
}

#[test]
fn activity_panel_uses_zh_catalog_labels() {
    let renderer = RatatuiInlineRenderer::with_width(100).with_language(crate::Language::ZhCn);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![
                ActivityRowModel {
                    id: "out-1",
                    kind: "output",
                    status: "captured",
                    subject: "tool-1",
                    summary: "stdout 已捕获；[Details] out-1",
                    tool: None,
                },
                ActivityRowModel {
                    id: "skill-1",
                    kind: "skill",
                    status: "failed",
                    subject: "linux_memory",
                    summary: "linux_memory 失败",
                    tool: None,
                },
                ActivityRowModel {
                    id: "tool-1",
                    kind: "tool",
                    status: "requested",
                    subject: "toolu-1",
                    summary: "run_shell_command 请求审批：$ df -h；[Details] tool-1",
                    tool: None,
                },
            ],
        })
        .join("\n");

    assert!(text.contains("活动"), "{text}");
    assert!(
        text.contains("Tool 输出: stdout 已捕获；[Details] out-1"),
        "{text}"
    );
    assert!(text.contains("技能 失败: linux_memory"), "{text}");
    assert!(
        text.contains("Tool 请求审批: run_shell_command 请求审批：$ df -h；[Details] tool-1"),
        "{text}"
    );
    assert!(!text.contains("Activity"), "{text}");
    assert!(!text.contains("Tool output:"), "{text}");
    assert!(!text.contains("Skill failed"), "{text}");
    assert!(!text.contains("Tool requested"), "{text}");
}

#[test]
fn activity_panel_prefers_typed_tool_row_in_en() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![ActivityRowModel {
                id: "tool-1",
                kind: "tool",
                status: "requested",
                subject: "toolu-read",
                summary: "Read requested: Cargo.toml; [Details] tool-1",
                tool: Some(ActivityToolRowModel {
                    kind: ToolPresentationKind::FileRead,
                    name: "Read",
                    primary: "Cargo.toml".into(),
                }),
            }],
        })
        .join("\n");

    assert!(text.contains("Read requested: Cargo.toml"), "{text}");
    assert!(!text.contains("Tool requested"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn activity_panel_prefers_typed_tool_row_in_zh() {
    let renderer = RatatuiInlineRenderer::with_width(100).with_language(crate::Language::ZhCn);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![ActivityRowModel {
                id: "tool-1",
                kind: "tool",
                status: "requested",
                subject: "toolu-shell",
                summary: "Bash 请求审批：$ df -h；[Details] tool-1",
                tool: Some(ActivityToolRowModel {
                    kind: ToolPresentationKind::ShellCommand,
                    name: "Bash",
                    primary: "$ df -h".into(),
                }),
            }],
        })
        .join("\n");

    assert!(text.contains("Shell 请求审批: $ df -h"), "{text}");
    assert!(!text.contains("Tool 请求审批"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert!(!text.contains("toolu-shell"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn activity_panel_preserves_interactive_handoff_action() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![ActivityRowModel {
                id: "handoff-1",
                kind: "shell",
                status: "error",
                subject: "toolu-shell",
                summary: "sudo: a terminal is required; may require foreground shell; [Send to shell] handoff-1; [Details] tool-2",
                tool: None,
            }],
        })
        .join("\n");

    assert!(text.contains("Shell: error"), "{text}");
    assert!(text.contains("sudo: a terminal is required"), "{text}");
    assert!(text.contains("[Send to shell]"), "{text}");
    assert!(text.contains("handoff-1"), "{text}");
    assert!(text.contains("[Details] tool-2"), "{text}");
    assert!(!text.contains("toolu-shell"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn activity_panel_wraps_long_rows_without_dropping_details_reference() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![ActivityRowModel {
                id: "out-1",
                kind: "output",
                status: "captured",
                subject: "req-7",
                summary: "stdout captured from approved request with a long summary; inspect [Details] out-1",
                tool: None,
            }],
        })
        .join("\n");

    assert!(text.contains("stdout captured"), "{text}");
    assert!(text.contains("[Details]"), "{text}");
    assert!(text.contains("out-1"), "{text}");
    assert!(!text.contains("req-7"), "{text}");
    assert_rendered_width(&text, 54);
}

#[test]
fn activity_panel_keeps_card_border_aligned_to_renderer_width() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![ActivityRowModel {
                id: "out-1",
                kind: "output",
                status: "captured",
                subject: "tool-1",
                summary: "stdout captured from 中文路径 🧪; inspect [Details] out-1",
                tool: None,
            }],
        })
        .join("\n");

    assert!(text.contains("Activity"), "{text}");
    assert!(text.contains("中文路径 🧪"), "{text}");
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

#[test]
fn activity_panel_write_preserves_ratatui_styles_for_terminal_output() {
    let renderer = RatatuiInlineRenderer {
        width: 100,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let mut output = Vec::new();

    renderer
        .write_activity_panel(
            &mut output,
            ActivityPanelModel {
                rows: vec![ActivityRowModel {
                    id: "out-1",
                    kind: "output",
                    status: "captured",
                    subject: "tool-1",
                    summary: "stdout captured; [Details] out-1",
                    tool: None,
                }],
            },
        )
        .expect("render activity panel");

    let text = String::from_utf8(output).expect("utf8 panel");
    let clean = strip_ansi_escape(&text);
    assert!(text.contains("\x1b["), "{text:?}");
    assert!(clean.contains("Activity"), "{clean}");
    assert!(clean.contains("Tool output"), "{clean}");
    assert!(!clean.contains("out-1 output"), "{clean}");
}

#[test]
fn plain_activity_panel_keeps_user_facing_row_text() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![ActivityRowModel {
                id: "out-1",
                kind: "output",
                status: "captured",
                subject: "tool-1",
                summary: "stdout captured; [Details] out-1",
                tool: None,
            }],
        })
        .join("\n");

    assert!(text.contains("Activity:"), "{text}");
    assert!(
        text.contains("Tool output: stdout captured; [Details] out-1"),
        "{text}"
    );
    assert!(!text.contains("out-1 output:"), "{text}");
    assert!(!text.contains('╭'), "{text}");
}

#[test]
fn plain_activity_panel_wraps_long_rows_without_dropping_details_reference() {
    let renderer = RatatuiInlineRenderer::plain_with_width(50);
    let text = renderer
        .activity_panel_lines(ActivityPanelModel {
            rows: vec![ActivityRowModel {
                id: "out-1",
                kind: "output",
                status: "captured",
                subject: "req-7",
                summary: "stdout captured from approved request with a long summary; inspect [Details] out-1",
                tool: None,
            }],
        })
        .join("\n");

    assert!(text.contains("Activity:"), "{text}");
    assert!(text.contains("Tool output: stdout captured from"), "{text}");
    assert!(
        text.contains("request with a long summary; inspect [Details]"),
        "{text}"
    );
    assert!(text.contains("out-1"), "{text}");
    assert!(!text.contains("req-7"), "{text}");
    assert!(!text.contains('╭'), "{text}");
    assert_rendered_width(&text, 50);
}

#[test]
fn activity_details_panel_renders_output_ref_and_detail_tail() {
    let renderer = RatatuiInlineRenderer::with_width(58);
    let text = renderer
        .activity_details_panel_lines(ActivityDetailsPanelModel {
            id: "out-1",
            run_id: "run-7",
            kind: "output",
            status: "captured",
            subject: "tool-1",
            summary: "stdout captured; [Details] out-1",
            detail: "tool: tool-1\nstream: stdout\nlines: 24\nref: /tmp/cosh-shell/out-1.txt\nline 1: fake tool output for details view\nline 24: fake tool output for details view",
        })
        .join("\n");

    assert!(text.contains("Activity details out-1"), "{text}");
    assert!(text.contains("output - stdout captured"), "{text}");
    assert!(text.contains("Run: run-7"), "{text}");
    assert!(text.contains("Detail:"), "{text}");
    assert!(text.contains("ref: /tmp/cosh-shell/out-1.txt"), "{text}");
    assert!(text.contains("line 24: fake tool output"), "{text}");
    assert!(!text.contains("id: out-1"), "{text}");
    assert_rendered_width(&text, 58);
}

#[test]
fn activity_details_panel_uses_zh_catalog_labels() {
    let renderer = RatatuiInlineRenderer::with_width(58).with_language(crate::Language::ZhCn);
    let text = renderer
        .activity_details_panel_lines(ActivityDetailsPanelModel {
            id: "out-1",
            run_id: "run-7",
            kind: "output",
            status: "captured",
            subject: "tool-1",
            summary: "stdout 已捕获；[Details] out-1",
            detail: "tool: tool-1\nstream: stdout\nref: /tmp/cosh-shell/out-1.txt",
        })
        .join("\n");

    assert!(text.contains("活动详情 out-1"), "{text}");
    assert!(
        text.contains("Tool 输出 - stdout 已捕获；[Details] out-1"),
        "{text}"
    );
    assert!(text.contains("运行: run-7"), "{text}");
    assert!(text.contains("详情:"), "{text}");
    assert!(text.contains("ref: /tmp/cosh-shell/out-1.txt"), "{text}");
    assert!(!text.contains("Activity details"), "{text}");
    assert!(!text.contains("output - stdout"), "{text}");
    assert!(!text.contains("Run:"), "{text}");
    assert!(!text.contains("Detail:"), "{text}");
}

#[test]
fn activity_details_panel_keeps_card_border_aligned_to_renderer_width() {
    let renderer = RatatuiInlineRenderer::with_width(58);
    let text = renderer
        .activity_details_panel_lines(ActivityDetailsPanelModel {
            id: "out-1",
            run_id: "run-中文-1",
            kind: "output",
            status: "captured",
            subject: "tool-1",
            summary: "stdout captured; [Details] out-1",
            detail:
                "ref: /tmp/cosh-shell/中文/out-1.txt\nline: CPU 🧪 output summary with long tail",
        })
        .join("\n");

    assert!(text.contains("Activity details out-1"), "{text}");
    assert!(text.contains("run-中文-1"), "{text}");
    assert!(text.contains("/tmp/cosh-shell/中文/out-1.txt"), "{text}");
    assert_rendered_width(&text, 58);
    assert_box_lines_aligned(&text, 58);
}

#[test]
fn plain_activity_details_panel_wraps_long_lines() {
    let renderer = RatatuiInlineRenderer::plain_with_width(50);
    let text = renderer
        .activity_details_panel_lines(ActivityDetailsPanelModel {
            id: "out-1",
            run_id: "run-with-a-very-long-identifier",
            kind: "output",
            status: "captured",
            subject: "tool-1",
            summary: "stdout captured; [Details] out-1",
            detail: "tool: tool-1\nstream: stdout\nref: /tmp/cosh-shell/very/long/path/out-1.txt\nline 24: fake tool output for details view with long trailing text",
        })
        .join("\n");

    assert!(text.contains("Activity details out-1"), "{text}");
    assert!(
        text.contains("output - stdout captured; [Details] out-1"),
        "{text}"
    );
    assert!(text.contains("tool-1"), "{text}");
    assert!(
        text.contains("Run: run-with-a-very-long-identifier"),
        "{text}"
    );
    assert!(text.contains("Detail:"), "{text}");
    assert!(
        text.contains("ref: /tmp/cosh-shell/very/long/path/out-1.txt"),
        "{text}"
    );
    assert!(
        text.contains("line 24: fake tool output for details view with"),
        "{text}"
    );
    assert!(text.contains("long trailing text"), "{text}");
    assert!(!text.contains('╭'), "{text}");
    assert_rendered_width(&text, 50);
}

#[test]
fn recommendation_panel_renders_display_only_commands() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let commands = vec!["pwd".to_string(), "echo $PATH".to_string()];
    let text = renderer
        .recommendation_panel_lines(RecommendationPanelModel {
            title: "Recommendations",
            summary: None,
            commands: &commands,
        })
        .join("\n");

    assert!(text.contains("Recommendations"), "{text}");
    assert!(text.contains("1. pwd"), "{text}");
    assert!(text.contains("2. echo $PATH"), "{text}");
    assert!(text.contains("│  1. pwd"), "{text}");
    assert!(text.contains("│  2. echo $PATH"), "{text}");
    assert!(!text.contains("[Copy] [Insert]"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert!(text.contains("display-only"), "{text}");
    assert!(!text.contains("/allow N"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn recommendation_panel_wraps_long_commands_without_dropping_tail() {
    let renderer = RatatuiInlineRenderer::with_width(56);
    let commands = vec![
        "cargo test --package cosh-shell --test raw_cli raw_cli_streaming_tool_approval_renders_before_agent_finishes -- --test-threads=1".to_string(),
    ];
    let text = renderer
        .recommendation_panel_lines(RecommendationPanelModel {
            title: "Recommendations",
            summary: None,
            commands: &commands,
        })
        .join("\n");

    assert!(text.contains("cargo test --package cosh-shell"), "{text}");
    assert!(text.contains("raw_cli_streaming_tool_approval"), "{text}");
    assert!(text.contains("--test-threads=1"), "{text}");
    assert!(!text.contains("[Copy] [Insert]"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert!(text.contains("display-only"), "{text}");
    assert_rendered_width(&text, 56);
}

#[test]
fn recommendation_panel_keeps_card_border_aligned_to_renderer_width() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let commands = vec![
        "cat /tmp/cosh-shell-中文-smoke.txt && echo 🧪 recommendation done".to_string(),
        "cargo test --package cosh-shell -- --test-threads=1".to_string(),
    ];
    let text = renderer
        .recommendation_panel_lines(RecommendationPanelModel {
            title: "Recommendations",
            summary: Some("Disk usage is high."),
            commands: &commands,
        })
        .join("\n");

    assert!(text.contains("Recommendations"), "{text}");
    assert!(text.contains("Disk usage is high."), "{text}");
    assert!(text.contains("中文-smoke.txt"), "{text}");
    assert!(text.contains("🧪"), "{text}");
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

#[test]
fn recommendation_panel_uses_zh_labels_without_translating_commands() {
    let renderer = RatatuiInlineRenderer::with_width(54).with_language(crate::Language::ZhCn);
    let commands = vec!["cat /tmp/cosh-shell-中文-smoke.txt".to_string()];
    let text = renderer
        .recommendation_panel_lines(RecommendationPanelModel {
            title: "建议下一步",
            summary: Some("磁盘使用率偏高。"),
            commands: &commands,
        })
        .join("\n");

    assert!(text.contains("建议下一步"), "{text}");
    assert!(text.contains("磁盘使用率偏高。"), "{text}");
    assert!(text.contains("仅展示：未执行任何命令"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert!(
        text.contains("cat /tmp/cosh-shell-中文-smoke.txt"),
        "{text}"
    );
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

#[test]
fn zh_cards_keep_40_and_80_column_widths() {
    for width in [40, 80] {
        let renderer =
            RatatuiInlineRenderer::with_width(width).with_language(crate::Language::ZhCn);
        let approval = renderer
            .approval_panel_lines(ApprovalPanelModel {
                id: "req-1",
                kind: "tool request",
                risk: "medium",
                reason: None,
                subject: "tool Bash",
                preview_label: "Tool 输入",
                preview: "cat /tmp/cosh-shell-中文-smoke.txt",
                queue_position: 1,
                queue_total: 1,
                next_label: None,
                selected_action: ApprovalPanelAction::Approve,
                expanded: true,
                turn_consent: false,
                turn_extension: false,
                deny_always_trust: false,
                irrecoverable: false,
                hook_warnings: Vec::new(),
            })
            .join("\n");
        assert!(approval.contains("审批"), "{approval}");
        assert!(approval.contains("/tmp/cosh-shell"), "{approval}");
        assert_rendered_width(&approval, width as usize);
        assert_box_lines_aligned(&approval, width as usize);

        let options = vec!["Green".to_string(), "Blue".to_string()];
        let question = renderer
            .question_panel_lines(QuestionPanelModel {
                id: "q-1",
                question: "Choose 中文 option",
                options: &options,
                selected_option: 0,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: true,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::None,
            })
            .join("\n");
        assert!(question.contains("Agent 问题"), "{question}");
        assert!(question.contains("[1] Green"), "{question}");
        assert_rendered_width(&question, width as usize);
        assert_box_lines_aligned(&question, width as usize);

        let commands = vec!["cat /tmp/cosh-shell-中文-smoke.txt".to_string()];
        let recommendation = renderer
            .recommendation_panel_lines(RecommendationPanelModel {
                title: "推荐",
                summary: None,
                commands: &commands,
            })
            .join("\n");
        assert!(recommendation.contains("推荐"), "{recommendation}");
        assert!(
            recommendation.contains("仅展示：未执行任何命令"),
            "{recommendation}"
        );
        assert!(!recommendation.contains("[Details]"), "{recommendation}");
        assert_rendered_width(&recommendation, width as usize);
        assert_box_lines_aligned(&recommendation, width as usize);
    }
}

#[test]
fn recommendation_panel_write_preserves_ratatui_styles_for_terminal_output() {
    let renderer = RatatuiInlineRenderer {
        width: 100,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let commands = vec!["pwd".to_string()];
    let mut output = Vec::new();

    renderer
        .write_recommendation_panel(
            &mut output,
            RecommendationPanelModel {
                title: "Recommendations",
                summary: None,
                commands: &commands,
            },
        )
        .expect("render recommendation panel");

    let text = String::from_utf8(output).expect("utf8 panel");
    let clean = strip_ansi_escape(&text);
    assert!(text.contains("\x1b["), "{text:?}");
    assert!(clean.contains("Recommendations"), "{clean}");
    assert!(clean.contains("1. pwd"), "{clean}");
    assert!(clean.contains("│  1. pwd"), "{clean}");
    assert!(!clean.contains("[Copy] [Insert]"), "{clean}");
    assert!(!clean.contains("[Details]"), "{clean}");
}

#[test]
fn plain_recommendation_panel_keeps_display_only_commands() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let commands = vec!["pwd".to_string(), "echo $PATH".to_string()];
    let text = renderer
        .recommendation_panel_lines(RecommendationPanelModel {
            title: "Recommendations",
            summary: None,
            commands: &commands,
        })
        .join("\n");

    assert!(text.contains("Recommendations:"), "{text}");
    assert!(text.contains("  1. pwd"), "{text}");
    assert!(text.contains("  2. echo $PATH"), "{text}");
    assert!(!text.contains("[Copy] [Insert]"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert!(text.contains("display-only"), "{text}");
    assert!(!text.contains("/allow N"), "{text}");
    assert!(!text.contains('╭'), "{text}");
    assert_rendered_width(&text, 80);
}

#[test]
fn plain_recommendation_panel_wraps_long_commands_without_dropping_tail() {
    let renderer = RatatuiInlineRenderer::plain_with_width(50);
    let commands = vec![
        "cargo test --package cosh-shell --test raw_cli raw_cli_streaming_tool_approval_renders_before_agent_finishes -- --test-threads=1".to_string(),
    ];
    let text = renderer
        .recommendation_panel_lines(RecommendationPanelModel {
            title: "Recommendations",
            summary: None,
            commands: &commands,
        })
        .join("\n");

    assert!(text.contains("Recommendations:"), "{text}");
    assert!(
        text.contains("  1. cargo test --package cosh-shell --test"),
        "{text}"
    );
    assert!(text.contains("     raw_cli"), "{text}");
    assert!(text.contains("raw_cli_streaming_tool_approval"), "{text}");
    assert!(text.contains("--test-threads=1"), "{text}");
    assert!(!text.contains("[Copy] [Insert]"), "{text}");
    assert!(!text.contains("[Details]"), "{text}");
    assert!(text.contains("display-only"), "{text}");
    assert!(!text.contains("/allow N"), "{text}");
    assert!(!text.contains('╭'), "{text}");
    assert_rendered_width(&text, 50);
}

#[test]
fn recommendation_action_panel_renders_display_only_receipt() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let text = renderer
        .recommendation_action_panel_lines(RecommendationActionPanelModel {
            title: "Recommendation selected",
            primary: "Selected recommendation 2".to_string(),
            command: Some("echo $PATH"),
            message: "Display-only: command was not executed; copy or re-enter it to run",
        })
        .join("\n");

    assert!(text.contains("Recommendation selected"), "{text}");
    assert!(text.contains("Selected recommendation 2"), "{text}");
    assert!(text.contains("echo $PATH"), "{text}");
    assert!(
        text.contains("Display-only: command was not executed"),
        "{text}"
    );
    assert_rendered_width(&text, 100);
}

#[test]
fn recommendation_action_panel_wraps_long_receipt_without_dropping_command() {
    let renderer = RatatuiInlineRenderer::with_width(56);
    let text = renderer
        .recommendation_action_panel_lines(RecommendationActionPanelModel {
            title: "Recommendation selected",
            primary: "Selected recommendation with a long display-only command".to_string(),
            command: Some(
                "cargo test --package cosh-shell --test raw_cli raw_cli_selects_recommendation_without_executing_it",
            ),
            message: "Display-only: command was not executed; copy or re-enter it to run",
        })
        .join("\n");

    assert!(text.contains("Selected recommendation"), "{text}");
    assert!(text.contains("cargo test --package cosh-shell"), "{text}");
    assert!(
        text.contains("raw_cli_selects_recommendation_without_executing_it"),
        "{text}"
    );
    assert!(text.contains("command was not executed"), "{text}");
    assert_rendered_width(&text, 56);
}

#[test]
fn recommendation_action_panel_keeps_card_border_aligned_to_renderer_width() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let text = renderer
        .recommendation_action_panel_lines(RecommendationActionPanelModel {
            title: "Recommendation selected",
            primary: "Selected command with 中文 path and emoji 🧪".to_string(),
            command: Some("cat /tmp/cosh-shell-中文-smoke.txt && echo 🧪 done"),
            message: "Display-only: command was not executed; copy or re-enter it to run",
        })
        .join("\n");

    assert!(text.contains("Recommendation selected"), "{text}");
    assert!(text.contains("中文 path"), "{text}");
    assert!(text.contains("/tmp/cosh-shell-中文-smoke.txt"), "{text}");
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

#[test]
fn recommendation_action_panel_write_preserves_ratatui_styles_for_terminal_output() {
    let renderer = RatatuiInlineRenderer {
        width: 100,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let mut output = Vec::new();

    renderer
        .write_recommendation_action_panel(
            &mut output,
            RecommendationActionPanelModel {
                title: "Recommendation selected",
                primary: "Selected recommendation 1".to_string(),
                command: Some("pwd"),
                message: "Display-only: command was not executed",
            },
        )
        .expect("render recommendation action panel");

    let text = String::from_utf8(output).expect("utf8 panel");
    let clean = strip_ansi_escape(&text);
    assert!(text.contains("\x1b["), "{text:?}");
    assert!(clean.contains("Recommendation selected"), "{clean}");
    assert!(clean.contains("pwd"), "{clean}");
}

#[test]
fn plain_recommendation_action_panel_keeps_receipt_text() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let text = renderer
        .recommendation_action_panel_lines(RecommendationActionPanelModel {
            title: "Recommendation selected",
            primary: "Selected recommendation 2".to_string(),
            command: Some("echo $PATH"),
            message: "Display-only: command was not executed",
        })
        .join("\n");

    assert!(text.contains("Recommendation selected:"), "{text}");
    assert!(text.contains("Selected recommendation 2"), "{text}");
    assert!(text.contains("  echo $PATH"), "{text}");
    assert!(text.contains("command was not executed"), "{text}");
    assert!(!text.contains('╭'), "{text}");
    assert_rendered_width(&text, 80);
}

#[test]
fn plain_recommendation_action_panel_wraps_long_receipt() {
    let renderer = RatatuiInlineRenderer::plain_with_width(50);
    let text = renderer
        .recommendation_action_panel_lines(RecommendationActionPanelModel {
            title: "Recommendation selected",
            primary: "Selected recommendation with a long display-only command".to_string(),
            command: Some(
                "cargo test --package cosh-shell --test raw_cli raw_cli_selects_recommendation_without_executing_it",
            ),
            message: "Display-only: command was not executed; copy or re-enter it to run",
        })
        .join("\n");

    assert!(text.contains("Recommendation selected:"), "{text}");
    assert!(
        text.contains("Selected recommendation with a long display-only"),
        "{text}"
    );
    assert!(text.contains("command"), "{text}");
    assert!(
        text.contains("  cargo test --package cosh-shell --test"),
        "{text}"
    );
    assert!(
        text.contains("raw_cli_selects_recommendation_without_executi"),
        "{text}"
    );
    assert!(text.contains("ng_it"), "{text}");
    assert!(
        text.contains("Display-only: command was not executed; copy or"),
        "{text}"
    );
    assert!(text.contains("re-enter it to run"), "{text}");
    assert!(!text.contains('╭'), "{text}");
    assert_rendered_width(&text, 50);
}

#[test]
fn renderer_snapshot_matrix_keeps_box_output_within_width() {
    let markdown = "### 结果\n\
        中文段落 with emoji ✅ and \u{1b}[31mred text\u{1b}[0m should wrap cleanly.\n\n\
        - inspect `/very/long/path/that/should/wrap/without/drifting`\n\
        - run cargo test --package cosh-shell --test raw_cli\n\n\
        ```bash\n\
        cargo test --package cosh-shell --test raw_cli -- --exact raw_cli_dumb_terminal_uses_plain_blocks\n\
        ```";
    let footer = "Commands are suggestions only; nothing was executed automatically.";

    for width in [40, 80, 120] {
        let renderer = RatatuiInlineRenderer::with_width(width);
        let mut output = Vec::new();
        renderer
            .write_agent_response(&mut output, markdown, Some(footer))
            .unwrap();
        let text = String::from_utf8(output).unwrap();

        assert_rendered_width(&text, width as usize);
        assert!(text.contains("red text"));
        assert!(!text.contains("\u{1b}[31m"));
        assert!(text.contains("Commands are suggestions only"));
    }
}

#[test]
fn notice_card_keeps_mode_footer_and_bottom_border() {
    let renderer = RatatuiInlineRenderer::with_width(40);
    let mut output = Vec::new();
    renderer
        .write_notice_panel(
            &mut output,
            NoticePanelModel {
                title: "Approval mode",
                body: vec!["Mode set to auto.".to_string()],
                footer: Some("Only low-risk read-only Bash tools can skip approval; risky requests still ask."),
            },
        )
        .unwrap();
    let text = String::from_utf8(output).unwrap();

    let footer_line = text
        .lines()
        .position(|line| line.contains("still ask."))
        .unwrap_or_else(|| panic!("mode footer should be visible:\n{text}"));
    let bottom_line = text
        .lines()
        .position(|line| line.starts_with('╰'))
        .unwrap_or_else(|| panic!("bottom border should be visible:\n{text}"));

    assert!(text.contains("Mode set to auto."), "{text}");
    assert!(
        text.contains("Only low-risk read-only Bash tools"),
        "{text}"
    );
    assert!(
        footer_line < bottom_line,
        "footer must render before bottom border:\n{text}"
    );
    assert_box_lines_aligned(&text, 40);
}

#[test]
fn renderer_snapshot_matrix_keeps_plain_output_within_width() {
    let body = vec![
        "Phase: requesting".to_string(),
        "中文状态 with emoji ✅ and \u{1b}[32mgreen text\u{1b}[0m should wrap".to_string(),
        "path: /very/long/path/that/should/wrap/without/drifting".to_string(),
    ];

    for width in [40, 80, 120] {
        let renderer = RatatuiInlineRenderer::plain_with_width(width);
        let mut output = Vec::new();
        renderer
            .write_notice_panel(
                &mut output,
                NoticePanelModel {
                    title: "Agent status",
                    body: body.clone(),
                    footer: Some(
                        "Commands are suggestions only; nothing was executed automatically.",
                    ),
                },
            )
            .unwrap();
        let text = String::from_utf8(output).unwrap();

        assert_rendered_width(&text, width as usize);
        assert!(text.contains("green text"));
        assert!(!text.contains("\u{1b}[32m"));
        assert!(!text.contains('╭'));
        assert!(!text.contains('│'));
        assert!(!text.contains('╰'));
    }
}

#[test]
fn health_banner_snapshot_matrix_keeps_rich_output_compact() {
    let report = warning_health_report();

    for width in [40, 80, 120] {
        let renderer = RatatuiInlineRenderer::with_width(width);
        let text = renderer
            .health_banner_lines(HealthBannerModel::new(&report))
            .join("\n");

        assert!(text.lines().count() <= 14, "{text}");
        assert_rendered_width(&text, width as usize);
        assert_box_lines_aligned(&text, width as usize);
        assert!(text.contains("Health check"), "{text}");
        assert!(text.contains("critical"), "{text}");
        assert!(text.contains("Load"), "{text}");
        assert!(text.contains("Resources"), "{text}");
        assert!(text.contains("1m 10.4 / 4 cores (2.6x)"), "{text}");
        assert!(!text.contains("Load  Load 1m"), "{text}");
        assert!(text.contains("Mem used"), "{text}");
        assert!(!text.contains("Mem avail 8% ▕"), "{text}");
        assert!(!text.contains("Memory 8%"), "{text}");
        if width >= 80 {
            assert!(text.contains("Disk / used"), "{text}");
            assert!(text.contains("Disk /data used"), "{text}");
        }
        let meter_widths = meter_cell_widths(&text);
        if width >= 120 {
            assert!(!meter_widths.is_empty(), "{text}");
            assert!(meter_widths.iter().all(|width| *width <= 8), "{text}");
        } else if width >= 80 {
            assert!(!meter_widths.is_empty(), "{text}");
            assert!(meter_widths.iter().all(|width| *width <= 6), "{text}");
        } else {
            assert!(meter_widths.is_empty(), "{text}");
        }
        assert!(!text.contains("██████████"), "{text}");
        assert!(text.contains("Findings"), "{text}");
        assert!(text.contains("Suggested Prompts"), "{text}");
        if width >= 80 {
            assert!(
                text.contains("You can type these prompts to the agent:"),
                "{text}"
            );
        } else {
            assert!(text.contains("You can type these prompts"), "{text}");
        }
        assert!(!text.contains("more finding"), "{text}");
        assert!(text.contains("available memory is low"), "{text}");
        let prompt_count = text.matches('›').count();
        if width >= 120 {
            assert_eq!(prompt_count, 3, "{text}");
        } else if width >= 80 {
            assert!(prompt_count >= 1, "{text}");
        } else {
            assert_eq!(prompt_count, 1, "{text}");
        }
        if width >= 80 {
            assert!(text.contains("Inspect the risky mount"), "{text}");
        }
        if width >= 120 {
            assert!(text.contains("Analyze memory pressure"), "{text}");
        } else {
            assert!(text.contains('›'), "{text}");
        }
        assert!(!text.contains("Next:"), "{text}");
        assert!(!text.contains(" · "), "{text}");
    }
}

#[test]
fn health_banner_matches_standard_panel_width_and_padding() {
    let report = warning_health_report();
    let renderer = RatatuiInlineRenderer::with_width(160);
    let health = renderer
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    let mut agent_output = Vec::new();
    renderer
        .write_agent_response(&mut agent_output, "Agent body", None)
        .unwrap();
    let agent = String::from_utf8(agent_output).unwrap();

    assert_box_lines_aligned(&health, 160);
    assert_box_lines_aligned(&agent, 160);
    assert!(
        health.lines().any(|line| line.starts_with("│ Resources")),
        "{health}"
    );
    assert!(
        !health.lines().any(|line| line.starts_with("│Resources")),
        "{health}"
    );
}

#[test]
fn health_banner_uses_zh_catalog_without_translating_commands() {
    let report = warning_health_report();

    for width in [40, 80, 120] {
        let renderer =
            RatatuiInlineRenderer::with_width(width).with_language(crate::Language::ZhCn);
        let text = renderer
            .health_banner_lines(HealthBannerModel::new(&report))
            .join("\n");

        assert!(text.lines().count() <= 14, "{text}");
        assert_rendered_width(&text, width as usize);
        assert_box_lines_aligned(&text, width as usize);
        assert!(text.contains("健康检查"), "{text}");
        assert!(text.contains("严重"), "{text}");
        assert!(text.contains("负载"), "{text}");
        assert!(text.contains("资源"), "{text}");
        assert!(text.contains("1分钟 10.4 / 4核（2.6倍）"), "{text}");
        assert!(!text.contains("负载  1分钟负载"), "{text}");
        assert!(text.contains("内存已用"), "{text}");
        if width >= 80 {
            assert!(text.contains("磁盘 /data 已用"), "{text}");
        }
        let meter_widths = meter_cell_widths(&text);
        if width >= 120 {
            assert!(!meter_widths.is_empty(), "{text}");
            assert!(meter_widths.iter().all(|width| *width <= 8), "{text}");
        } else if width >= 80 {
            assert!(!meter_widths.is_empty(), "{text}");
            assert!(meter_widths.iter().all(|width| *width <= 6), "{text}");
        } else {
            assert!(meter_widths.is_empty(), "{text}");
        }
        assert!(!text.contains("██████████"), "{text}");
        assert!(text.contains("发现的问题"), "{text}");
        assert!(text.contains("建议下一步"), "{text}");
        if width >= 80 {
            assert!(text.contains("以下提示词可直接输入给 Agent："), "{text}");
        } else {
            assert!(text.contains("以下提示词可直接输入"), "{text}");
        }
        assert!(!text.contains("另有"), "{text}");
        if width >= 120 {
            assert!(text.contains("可用内存偏低"), "{text}");
        }
        let prompt_count = text.matches('›').count();
        if width >= 120 {
            assert_eq!(prompt_count, 3, "{text}");
        } else if width >= 80 {
            assert!(prompt_count >= 1, "{text}");
        } else {
            assert_eq!(prompt_count, 1, "{text}");
        }
        if width >= 80 {
            assert!(text.contains("检查高风险挂载点"), "{text}");
        }
        if width >= 120 {
            assert!(text.contains("分析内存压力"), "{text}");
        } else {
            assert!(text.contains('›'), "{text}");
        }
        assert!(!text.contains("Health check"), "{text}");
        assert!(!text.contains("Next:"), "{text}");
    }
}

#[test]
fn health_banner_long_mount_ellipsis_and_gauge_semantics() {
    let mut report = warning_health_report();
    let long_mount = "/home/quejianming.linux/.copilot-shell/cache/runtime/artifacts";
    for fact in &mut report.facts {
        if fact.key == "filesystem.riskiest_mount" {
            fact.value = HealthFactValue::String(long_mount.to_string());
        }
    }

    let text = RatatuiInlineRenderer::with_width(120)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert_rendered_width(&text, 120);
    assert!(text.contains("..."), "{text}");
    assert!(!text.contains(long_mount), "{text}");
    assert!(text.contains("Mem used 92% ▕████████▏"), "{text}");
    assert!(!text.contains("Mem avail 8% ▕"), "{text}");
    assert!(!text.contains("CPU load 2.6x/core ["), "{text}");
}

#[test]
fn health_banner_hides_riskiest_disk_metric_without_disk_finding() {
    let mut report = warning_health_report();
    report
        .findings
        .retain(|finding| finding.title_id != HealthMessageId::HealthFindingDiskHigh);
    report.try_items.retain(|item| item.finding_id != "J09");
    report.recompute_overall_severity();

    let text = RatatuiInlineRenderer::with_width(120)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert!(text.contains("Disk / used 41%"), "{text}");
    assert!(!text.contains("Disk /data used"), "{text}");
    assert!(!text.contains("Inspect the risky mount"), "{text}");
}

#[test]
fn health_banner_wraps_prompt_suggestions_without_ellipsis_truncation() {
    let report = warning_health_report();

    let text = RatatuiInlineRenderer::with_width(40)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    let compact = compact_without_box_chars(&text);

    assert_rendered_width(&text, 40);
    assert!(text.lines().count() <= 14, "{text}");
    assert!(text.contains("Suggested Prompts"), "{text}");
    assert!(
        compact.contains("Inspect the risky mount and suggest"),
        "{text}"
    );
    if text.lines().count() < 14 {
        assert!(compact.contains("safe disk cleanup targets."), "{text}");
    }
    assert!(!text.contains("..."), "{text}");
}

#[test]
fn health_banner_oom_prompt_wraps_fully_at_narrow_width() {
    let report = oom_health_report();

    let text = RatatuiInlineRenderer::with_width(40)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    let compact = compact_without_box_chars(&text);

    assert_rendered_width(&text, 40);
    assert!(text.lines().count() <= 14, "{text}");
    assert!(text.contains("Suggested Prompts"), "{text}");
    assert!(compact.contains("memory state around the event."), "{text}");
    assert!(!text.contains("..."), "{text}");
}

#[test]
fn health_banner_shows_cpu_used_only_when_utilization_fact_exists() {
    let without_cpu_used = RatatuiInlineRenderer::with_width(120)
        .health_banner_lines(HealthBannerModel::new(&warning_health_report()))
        .join("\n");
    assert!(!without_cpu_used.contains("CPU used"), "{without_cpu_used}");
    assert!(!without_cpu_used.contains("CPU 已用"), "{without_cpu_used}");

    let mut report = warning_health_report();
    report
        .facts
        .push(health_float_fact("cpu.utilization_ratio", 0.37));

    let text = RatatuiInlineRenderer::with_width(120)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    assert!(text.contains("CPU used 37% ▕███░░░░░▏"), "{text}");
    assert!(text.contains("1m 10.4 / 4 cores (2.6x)"), "{text}");
    assert!(!text.contains("Load  Load 1m"), "{text}");

    let zh_text = RatatuiInlineRenderer::with_width(120)
        .with_language(crate::Language::ZhCn)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    assert!(zh_text.contains("CPU 已用 37% ▕███░░░░░▏"), "{zh_text}");
    assert!(zh_text.contains("1分钟 10.4 / 4核（2.6倍）"), "{zh_text}");
    assert!(!zh_text.contains("负载  1分钟负载"), "{zh_text}");
}

#[test]
fn health_banner_recent_oom_uses_compact_evidence_without_raw_fact_dump() {
    let report = oom_health_report();

    let text = RatatuiInlineRenderer::with_width(120)
        .with_language(crate::Language::ZhCn)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert_rendered_width(&text, 120);
    assert!(text.contains("最近一次 OOM"), "{text}");
    assert!(text.contains("OOM"), "{text}");
    assert!(text.contains("python3"), "{text}");
    assert!(
        compact_without_box_chars(&text).contains("PID 49991"),
        "{text}"
    );
    assert!(text.contains("cgroup 内存限制触发"), "{text}");
    assert!(!text.contains("CONSTRAINT_MEMCG"), "{text}");
    assert!(!text.contains("CONSTRAINT_"), "{text}");
    assert!(text.contains("建议下一步"), "{text}");
    assert!(text.contains("帮我分析最近一次 OOM 的原因"), "{text}");
    assert!(!text.contains("解释当前资源压力"), "{text}");
    assert!(!text.contains("OOM age"), "{text}");
    assert!(!text.contains("process "), "{text}");
    assert!(!text.contains("pid "), "{text}");
    assert!(!text.contains("constraint "), "{text}");
    assert!(!text.contains("task cgroup"), "{text}");
    assert!(!text.contains("oom cgroup"), "{text}");
}

#[test]
fn health_banner_recent_oom_scope_label_id_wins_over_raw_constraint() {
    let mut report = oom_health_report();
    for fact in &mut report.facts {
        if fact.key == "kernel.oom_latest_constraint" {
            fact.value = HealthFactValue::String("CONSTRAINT_NONE".to_string());
        }
    }
    report.facts.push(health_string_fact(
        "kernel.oom_latest_scope_label_id",
        "memcg",
    ));

    let text = RatatuiInlineRenderer::with_width(120)
        .with_language(crate::Language::ZhCn)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert!(text.contains("cgroup 内存限制触发"), "{text}");
    assert!(!text.contains("整机内存不足触发"), "{text}");
    assert!(!text.contains("CONSTRAINT_"), "{text}");
}

#[test]
fn health_banner_styled_output_keeps_finding_body_primary() {
    let report = oom_health_report();
    let renderer = RatatuiInlineRenderer {
        width: 120,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let mut output = Vec::new();

    renderer
        .write_health_banner(&mut output, HealthBannerModel::new(&report))
        .expect("render styled health banner");

    let text = String::from_utf8(output).expect("utf8 health banner");
    let clean = strip_ansi_escape(&text);
    let body = "the latest OOM has already happened";
    assert!(clean.contains(body), "{clean}");
    assert!(
        text.contains(";31m") || text.contains("[31m"),
        "critical border/title should carry red styling: {text:?}"
    );
    let active_style = active_ansi_style_before(&text, body);
    assert!(
        !active_style.contains("31") && !active_style.contains("33"),
        "finding body should not inherit severity color, active style={active_style:?}\n{text:?}"
    );
}

#[test]
fn health_banner_styled_labels_are_readable_not_dim_only() {
    let report = oom_health_report();
    let renderer = RatatuiInlineRenderer {
        width: 120,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let mut output = Vec::new();

    renderer
        .write_health_banner(&mut output, HealthBannerModel::new(&report))
        .expect("render styled health banner");

    let text = String::from_utf8(output).expect("utf8 health banner");
    let clean = strip_ansi_escape(&text);
    assert!(clean.contains("Load"), "{clean}");
    assert!(clean.contains("Findings"), "{clean}");
    assert!(clean.contains("Suggested Prompts"), "{clean}");
    assert!(
        clean.contains("You can type these prompts to the agent:"),
        "{clean}"
    );

    for label in [
        "Load",
        "Findings",
        "Suggested Prompts",
        "You can type these prompts to the agent:",
    ] {
        let style = active_ansi_style_before(&text, label);
        assert!(
            !style.contains("90") && !style.contains("31") && !style.contains("33"),
            "{label} should be readable muted text, active style={style:?}\n{text:?}"
        );
    }
}

#[test]
fn health_banner_oom_prompts_are_cause_oriented_in_both_languages() {
    let mut report = oom_health_report();
    report.try_items = vec![health_try_for(
        "T07",
        HealthMessageId::HealthTryInspectProcessMemory,
        HealthMessageId::HealthTryReasonRecentOom,
        120,
        "J11",
    )];
    report.try_items[0]
        .prompt_args
        .insert("process".to_string(), "python3".to_string());

    let zh_text = RatatuiInlineRenderer::with_width(120)
        .with_language(crate::Language::ZhCn)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    assert!(
        zh_text.contains("帮我分析最近一次 OOM 为什么杀掉 python3"),
        "{zh_text}"
    );
    assert!(zh_text.contains("cgroup 和内存上限"), "{zh_text}");
    assert!(!zh_text.contains("检查 python3 的内存占用"), "{zh_text}");
    assert!(!zh_text.contains("是否导致了最新 OOM"), "{zh_text}");

    let en_text = RatatuiInlineRenderer::with_width(120)
        .with_language(crate::Language::EnUs)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    let en_compact = compact_words(&en_text);
    assert!(
        en_compact.contains("Help me analyze why the latest OOM killed python3"),
        "{en_text}"
    );
    assert!(
        en_compact.contains("cgroup scope and memory limits"),
        "{en_text}"
    );
    assert!(
        !en_text.contains("whether it caused the latest OOM"),
        "{en_text}"
    );
    assert!(!en_text.contains("current pressure"), "{en_text}");
}

#[test]
fn health_banner_plain_fallback_keeps_content_without_box_art() {
    let report = warning_health_report();
    let renderer = RatatuiInlineRenderer::plain_with_width(50);

    let text = renderer
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert!(text.contains("Health check:"), "{text}");
    assert!(text.contains("critical"), "{text}");
    assert!(text.contains("Mem used"), "{text}");
    assert!(text.contains("Findings"), "{text}");
    assert!(text.contains("Suggested Prompts"), "{text}");
    assert!(
        text.contains("You can type these prompts to the agent:"),
        "{text}"
    );
    assert!(!text.contains("▕"), "{text}");
    assert!(!text.contains('╭'), "{text}");
    assert!(!text.contains('│'), "{text}");
    assert_rendered_width(&text, 50);
}

#[test]
fn health_banner_compresses_healthy_report() {
    let mut report = HealthScanReport::new("health-ok", 0);
    report.elapsed_ms = 24;
    report.health_score = Some(98);
    report.facts = vec![
        health_float_fact("cpu.load_per_core_1m", 0.2),
        health_float_fact("cpu.load_1m", 0.8),
        health_float_fact("cpu.cores", 4.0),
        health_float_fact("memory.available_ratio", 0.62),
        health_float_fact("memory.used_ratio", 0.38),
        health_float_fact("memory.swap_used_ratio", 0.0),
        health_string_fact("filesystem.riskiest_mount", "/cache"),
        health_float_fact("filesystem.max_used_ratio", 0.69),
        health_float_fact("filesystem.root_used_ratio", 0.41),
    ];
    report.recompute_overall_severity();

    let text = RatatuiInlineRenderer::with_width(80)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert!(text.lines().count() <= 2, "{text}");
    assert!(text.contains("Health: ok"), "{text}");
    assert!(text.contains("Load 1m 0.8 / 4 cores (0.2x)"), "{text}");
    assert!(text.contains("Mem used 38%"), "{text}");
    assert!(!text.contains("Mem avail"), "{text}");
    assert!(!text.contains("Swap used 0%"), "{text}");
    assert!(compact_words(&text).contains("Disk / used 41%"), "{text}");
    assert!(!text.contains("/cache"), "{text}");
    assert!(!text.contains("98/100"), "{text}");
    assert!(!text.contains("▕"), "{text}");
    assert!(!text.contains("Suggested Prompt"), "{text}");
}

#[test]
fn health_banner_caps_try_lines_and_hides_suppressed_try_items() {
    let mut report = warning_health_report();
    report.findings.clear();
    report.try_items.push(health_try(
        "T04",
        HealthMessageId::HealthTryInspectHighLoad,
        HealthMessageId::HealthTryReasonHighLoad,
        70,
    ));
    report.overall_severity = HealthSeverity::Warning;

    let text = RatatuiInlineRenderer::with_width(80)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    assert_eq!(
        text.matches("You can type these prompts to the agent:")
            .count(),
        1,
        "{text}"
    );
    assert!(text.matches('›').count() <= 3, "{text}");

    report.findings = vec![health_finding(
        "J06",
        HealthSeverity::Warning,
        HealthMessageId::HealthFindingMemoryAvailableLow,
    )];
    report.try_items.clear();
    let suppressed_text = RatatuiInlineRenderer::with_width(80)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    assert!(suppressed_text.contains("warning"), "{suppressed_text}");
    assert!(
        !suppressed_text.contains("Suggested Prompt"),
        "{suppressed_text}"
    );
}

#[test]
fn health_banner_filters_prompts_for_hidden_findings() {
    let mut report = warning_health_report();
    report.findings.push(health_finding(
        "J02",
        HealthSeverity::Warning,
        HealthMessageId::HealthFindingCpuLoadHigh,
    ));
    report.findings.push(health_finding(
        "J99",
        HealthSeverity::Warning,
        HealthMessageId::HealthFindingServiceFailed,
    ));
    report.try_items.push(health_try_for(
        "T99",
        HealthMessageId::HealthTryInspectServiceStatus,
        HealthMessageId::HealthTryReasonServiceState,
        1000,
        "J99",
    ));
    report.recompute_overall_severity();

    let text = RatatuiInlineRenderer::with_width(120)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert!(text.contains("Findings"), "{text}");
    assert_eq!(text.matches('›').count(), 3, "{text}");
    assert!(!text.contains("configured service"), "{text}");
    assert!(
        !text.contains("Inspect the configured service state"),
        "{text}"
    );
    assert!(!text.contains("service unit"), "{text}");
    if text.contains("Suggested Prompts") {
        assert!(text.contains('›'), "{text}");
    }
}

#[test]
fn health_banner_service_only_prompt_aligns_with_service_finding() {
    let mut report = HealthScanReport::new("health-service", 0);
    report.elapsed_ms = 8;
    report.facts = vec![
        health_float_fact("cpu.load_per_core_1m", 0.1),
        health_float_fact("cpu.load_1m", 0.4),
        health_float_fact("cpu.cores", 4.0),
        health_float_fact("memory.used_ratio", 0.31),
        health_float_fact("filesystem.root_used_ratio", 0.22),
        health_string_fact("service.redis.service.status", "failed"),
    ];
    let mut service_finding = health_finding(
        "J15:redis.service",
        HealthSeverity::Critical,
        HealthMessageId::HealthFindingServiceFailed,
    );
    service_finding
        .detail_args
        .insert("service".to_string(), "redis.service".to_string());
    service_finding
        .detail_args
        .insert("observed".to_string(), "failed".to_string());
    service_finding
        .detail_args
        .insert("expected".to_string(), "active".to_string());
    service_finding.evidence_fact_ids = vec!["service.redis.service.status".to_string()];
    report.findings = vec![service_finding];
    report.try_items = vec![health_try_for(
        "T20",
        HealthMessageId::HealthTryInspectServiceStatus,
        HealthMessageId::HealthTryReasonServiceState,
        100,
        "J15:redis.service",
    )];
    report.recompute_overall_severity();

    let text = RatatuiInlineRenderer::with_width(120)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert!(text.contains("service unit redis.service"), "{text}");
    assert!(text.contains("observed failed, expected active"), "{text}");
    assert!(
        text.contains("Inspect the configured service state"),
        "{text}"
    );
    assert!(!text.contains("OOM"), "{text}");

    let zh_text = RatatuiInlineRenderer::with_width(120)
        .with_language(crate::Language::ZhCn)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");
    assert!(zh_text.contains("服务单元 redis.service"), "{zh_text}");
    assert!(zh_text.contains("当前 failed，预期 active"), "{zh_text}");
    assert!(zh_text.contains("检查配置服务状态"), "{zh_text}");
}

#[test]
fn health_banner_merges_degraded_unavailable_checks() {
    let mut report = HealthScanReport::new("health-degraded", 0);
    report.health_score = Some(80);
    report.unavailable.push(UnavailableCollector {
        collector: HealthCollector::KernelSignal,
        reason: HealthUnavailableReason::PermissionDenied,
        severity: HealthSeverity::Degraded,
        elapsed_ms: 3,
    });
    report.recompute_overall_severity();

    let text = RatatuiInlineRenderer::with_width(80)
        .health_banner_lines(HealthBannerModel::new(&report))
        .join("\n");

    assert!(text.contains("degraded"), "{text}");
    assert!(text.contains("Unavailable:"), "{text}");
    assert!(text.contains("Signal"), "{text}");
    assert!(text.contains("permission denied"), "{text}");
    assert!(text.contains("80/100"), "{text}");
    assert!(text.contains("▕"), "{text}");
}

#[test]
fn streaming_snapshot_keeps_footer_within_width() {
    for width in [40, 80, 120] {
        let renderer = RatatuiInlineRenderer::with_width(width);
        let mut stream = renderer.stream_agent();
        let mut output = Vec::new();

        stream
            .write_delta(
                &mut output,
                "streaming 中文 token with \u{1b}[33mcolored text\u{1b}[0m and a long path /tmp/cosh-shell/snapshot/matrix",
            )
            .unwrap();
        stream
            .finish(
                &mut output,
                Some("Commands are suggestions only; nothing was executed automatically."),
            )
            .unwrap();
        let text = String::from_utf8(output).unwrap();

        assert_rendered_width(&text, width as usize);
        assert!(text.contains("colored"));
        assert!(!text.contains("\u{1b}[33m"));
    }
}

#[test]
fn streaming_card_keeps_ambiguous_and_combining_widths_aligned() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream
        .write_delta(
            &mut output,
            "status · waiting → café e\u{301} ⚠️ 🤦🏽‍♂️ 中文 🧪 done",
        )
        .unwrap();
    stream.finish(&mut output, None).unwrap();
    let text = String::from_utf8(output).unwrap();

    assert!(text.contains("status · waiting"), "{text}");
    assert!(text.contains("café"), "{text}");
    for line in text.lines().filter(|line| !line.is_empty()) {
        assert_eq!(
            snapshot_width(line),
            54,
            "streaming card line should keep a stable right border: {line:?}\n{text}"
        );
    }

    let renderer = RatatuiInlineRenderer::with_width(40);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream.write_delta(&mut output, &"中".repeat(18)).unwrap();
    stream.write_delta(&mut output, "alpha").unwrap();
    stream.finish(&mut output, None).unwrap();
    let text = String::from_utf8(output).unwrap();
    let expected_line = "中".repeat(18);
    let first_content = text
        .lines()
        .find_map(|line| line.strip_prefix("│ "))
        .and_then(|line| line.strip_suffix(" │"));

    assert_eq!(
        first_content,
        Some(expected_line.as_str()),
        "streaming words should preserve an exactly filled line: {text}"
    );

    for punctuation in [
        '。', '，', '、', '；', '：', '！', '？', ')', ']', '}', '）', '】', '》', '〉', '」',
        '』', '”', '’',
    ] {
        let renderer = RatatuiInlineRenderer::with_width(40);
        let mut stream = renderer.stream_agent();
        let mut output = Vec::new();

        stream.write_delta(&mut output, &"中".repeat(18)).unwrap();
        stream
            .write_delta(&mut output, &punctuation.to_string())
            .unwrap();
        stream.finish(&mut output, None).unwrap();
        let text = String::from_utf8(output).unwrap();
        let bound_prefix = format!("中{punctuation}");

        assert!(text.contains(punctuation), "{text}");
        assert!(
            text.lines().any(|line| line
                .strip_prefix("│ ")
                .is_some_and(|content| content.starts_with(&bound_prefix))),
            "streaming continuation should bind its punctuation: {text}"
        );
        for line in text.lines().filter(|line| !line.is_empty()) {
            assert_eq!(
                snapshot_width(line),
                40,
                "streaming punctuation should keep the right border aligned: {line:?}\n{text}"
            );
        }
    }

    let renderer = RatatuiInlineRenderer::with_width(40);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream.write_delta(&mut output, &"中".repeat(17)).unwrap();
    stream.write_delta(&mut output, "。").unwrap();
    stream.write_delta(&mut output, "”").unwrap();
    stream.finish(&mut output, None).unwrap();
    let text = String::from_utf8(output).unwrap();

    assert!(
        text.lines().any(|line| line
            .strip_prefix("│ ")
            .is_some_and(|content| content.starts_with("中。”"))),
        "streaming continuation should keep consecutive punctuation bound: {text}"
    );
    for line in text.lines().filter(|line| !line.is_empty()) {
        assert_eq!(
            snapshot_width(line),
            40,
            "consecutive punctuation should keep the right border aligned: {line:?}\n{text}"
        );
    }

    let renderer = RatatuiInlineRenderer::with_width(40);
    let mut stream = renderer.stream_agent();
    let mut output = Vec::new();

    stream.write_delta(&mut output, "中").unwrap();
    stream.write_delta(&mut output, &"。".repeat(40)).unwrap();
    stream.finish(&mut output, None).unwrap();
    let text = String::from_utf8(output).unwrap();

    assert_eq!(text.matches('。').count(), 40, "{text}");
    for line in text.lines().filter(|line| !line.is_empty()) {
        assert_eq!(
            snapshot_width(line),
            40,
            "long punctuation runs should keep the right border aligned: {line:?}\n{text}"
        );
    }
}

#[test]
fn markdown_stream_keeps_ambiguous_and_combining_widths_aligned() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let mut stream = renderer.stream_markdown_agent();
    let mut output = Vec::new();

    stream
        .write_delta(
            &mut output,
            "渲染检查\n\n- status · waiting → café e\u{301}\n- warning ⚠️ and emoji 🤦🏽‍♂️\n\n",
        )
        .unwrap();
    stream.finish(&mut output, None).unwrap();
    let text = String::from_utf8(output).unwrap();

    assert!(text.contains("status · waiting"), "{text}");
    assert!(text.contains("warning"), "{text}");
    assert_box_lines_aligned(&text, 54);
}

fn assert_rendered_width(output: &str, max_width: usize) {
    for line in output.lines() {
        let width = snapshot_width(line);
        assert!(
            width <= max_width,
            "line width {width} exceeds {max_width}: {line:?}\n{output}"
        );
    }
}

fn assert_box_lines_aligned(output: &str, expected_width: usize) {
    for line in output.lines() {
        let width = snapshot_width(line);
        assert_eq!(
            width, expected_width,
            "box line width {width} differs from {expected_width}: {line:?}\n{output}"
        );
    }
}

fn assert_box_lines_same_width(output: &str) -> usize {
    let expected_width = output
        .lines()
        .find(|line| !line.is_empty())
        .map(snapshot_width)
        .expect("at least one rendered line");
    assert_box_lines_aligned(output, expected_width);
    expected_width
}

fn snapshot_width(line: &str) -> usize {
    Span::raw(strip_ansi_escape(line)).width()
}

fn meter_cell_widths(text: &str) -> Vec<usize> {
    let mut widths = Vec::new();
    let mut inside = false;
    let mut width = 0;
    for ch in text.chars() {
        if ch == '▕' {
            inside = true;
            width = 0;
            continue;
        }
        if inside && ch == '▏' {
            widths.push(width);
            inside = false;
            continue;
        }
        if inside {
            width += 1;
        }
    }
    widths
}

fn active_ansi_style_before(text: &str, needle: &str) -> String {
    let index = text
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?} in {text:?}"));
    text[..index]
        .rsplit("\u{1b}[")
        .next()
        .and_then(|part| part.split('m').next())
        .unwrap_or("")
        .to_string()
}

fn line_index(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle:?} in {lines:?}"))
}

fn warning_health_report() -> HealthScanReport {
    let mut report = HealthScanReport::new("health-warning", 100);
    report.elapsed_ms = 145;
    report.health_score = Some(62);
    report.facts = vec![
        health_float_fact("cpu.load_per_core_1m", 2.6),
        health_float_fact("cpu.load_per_core_5m", 1.4),
        health_float_fact("cpu.load_1m", 10.4),
        health_float_fact("cpu.load_5m", 5.6),
        health_float_fact("cpu.cores", 4.0),
        health_float_fact("memory.available_ratio", 0.08),
        health_float_fact("memory.used_ratio", 0.92),
        health_float_fact("memory.swap_used_ratio", 0.55),
        health_string_fact("filesystem.riskiest_mount", "/data"),
        health_float_fact("filesystem.max_used_ratio", 0.96),
        health_float_fact("filesystem.available_gib", 2.0),
        health_float_fact("filesystem.root_used_ratio", 0.41),
    ];
    report.findings = vec![
        health_finding(
            "J06",
            HealthSeverity::Warning,
            HealthMessageId::HealthFindingMemoryAvailableLow,
        ),
        health_finding(
            "J09",
            HealthSeverity::Critical,
            HealthMessageId::HealthFindingDiskHigh,
        ),
    ];
    report.try_items = vec![
        health_try(
            "T01",
            HealthMessageId::HealthTryAnalyzeMemoryPressure,
            HealthMessageId::HealthTryReasonMemoryLow,
            100,
        ),
        health_try_for(
            "T02",
            HealthMessageId::HealthTryInspectDiskUsage,
            HealthMessageId::HealthTryReasonDiskHigh,
            90,
            "J09",
        ),
        health_try(
            "T03",
            HealthMessageId::HealthTryCheckSwapPressure,
            HealthMessageId::HealthTryReasonSwapWithContext,
            80,
        ),
    ];
    report.recompute_overall_severity();
    report
}

fn oom_health_report() -> HealthScanReport {
    let mut report = HealthScanReport::new("health-oom", 100);
    report.elapsed_ms = 9;
    report.facts = vec![
        health_float_fact("cpu.load_per_core_1m", 0.1),
        health_float_fact("cpu.load_1m", 0.4),
        health_float_fact("cpu.cores", 4.0),
        health_float_fact("memory.available_ratio", 0.11),
        health_float_fact("memory.used_ratio", 0.89),
        health_float_fact("memory.swap_used_ratio", 0.0),
        health_float_fact("filesystem.root_used_ratio", 0.31),
        health_float_fact("kernel.oom_latest_age_seconds", 3.0),
        health_string_fact("kernel.oom_killed_process", "python3"),
        health_float_fact("kernel.oom_latest_pid", 49991.0),
        health_string_fact("kernel.oom_latest_constraint", "CONSTRAINT_MEMCG"),
        health_string_fact(
            "kernel.oom_latest_oom_cgroup",
            "/user.slice/user-1000.slice/session-5.scope",
        ),
    ];
    report.findings = vec![health_finding(
        "J11",
        HealthSeverity::Critical,
        HealthMessageId::HealthFindingRecentOom,
    )];
    report.try_items = vec![health_try_for(
        "T11",
        HealthMessageId::HealthTryCheckRecentOom,
        HealthMessageId::HealthTryReasonRecentOom,
        100,
        "J11",
    )];
    report.recompute_overall_severity();
    report
}

fn health_float_fact(key: &str, value: f64) -> HealthFact {
    HealthFact {
        id: key.to_string(),
        category: HealthFactCategory::Memory,
        key: key.to_string(),
        value: HealthFactValue::Float(value),
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 0,
    }
}

fn health_string_fact(key: &str, value: &str) -> HealthFact {
    HealthFact {
        id: key.to_string(),
        category: HealthFactCategory::Disk,
        key: key.to_string(),
        value: HealthFactValue::String(value.to_string()),
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 0,
    }
}

fn compact_words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compact_without_box_chars(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if matches!(ch, '│' | '╭' | '╰' | '─' | '╮' | '╯') {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn health_finding(id: &str, severity: HealthSeverity, title_id: HealthMessageId) -> HealthFinding {
    let evidence_fact_ids = match title_id {
        HealthMessageId::HealthFindingMemoryAvailableLow => {
            vec!["memory.available_ratio".to_string()]
        }
        HealthMessageId::HealthFindingDiskHigh => vec![
            "filesystem.max_used_ratio".to_string(),
            "filesystem.available_gib".to_string(),
            "filesystem.riskiest_mount".to_string(),
        ],
        HealthMessageId::HealthFindingRecentOom => vec![
            "kernel.oom_latest_age_seconds".to_string(),
            "kernel.oom_killed_process".to_string(),
            "kernel.oom_latest_pid".to_string(),
            "kernel.oom_latest_constraint".to_string(),
            "kernel.oom_latest_oom_cgroup".to_string(),
        ],
        _ => Vec::new(),
    };
    HealthFinding {
        id: id.to_string(),
        severity,
        category: HealthFindingCategory::Anomaly,
        title_id,
        detail_id: None,
        detail_args: BTreeMap::new(),
        evidence_fact_ids,
        suggested_try_ids: Vec::new(),
    }
}

fn health_try(
    id: &str,
    label_id: HealthMessageId,
    reason_id: HealthMessageId,
    score: i32,
) -> HealthTryItem {
    health_try_for(id, label_id, reason_id, score, "J06")
}

fn health_try_for(
    id: &str,
    label_id: HealthMessageId,
    reason_id: HealthMessageId,
    score: i32,
    finding_id: &str,
) -> HealthTryItem {
    HealthTryItem {
        id: id.to_string(),
        label_id,
        label_args: BTreeMap::new(),
        prompt_id: test_prompt_id(label_id),
        prompt_args: BTreeMap::new(),
        kind: HealthTryKind::AskAgent,
        command: None,
        reason_id,
        reason_args: BTreeMap::new(),
        score,
        finding_id: finding_id.to_string(),
    }
}

fn test_prompt_id(label_id: HealthMessageId) -> Option<HealthMessageId> {
    match label_id {
        HealthMessageId::HealthTryAnalyzeMemoryPressure => {
            Some(HealthMessageId::HealthPromptAnalyzeMemoryPressure)
        }
        HealthMessageId::HealthTryCheckSwapPressure => {
            Some(HealthMessageId::HealthPromptCheckSwapPressure)
        }
        HealthMessageId::HealthTryCheckRecentOom => {
            Some(HealthMessageId::HealthPromptCheckRecentOom)
        }
        HealthMessageId::HealthTryInspectDiskUsage => {
            Some(HealthMessageId::HealthPromptInspectDiskUsage)
        }
        HealthMessageId::HealthTryInspectServiceStatus => {
            Some(HealthMessageId::HealthPromptInspectServiceStatus)
        }
        HealthMessageId::HealthTryInspectHighLoad => {
            Some(HealthMessageId::HealthPromptInspectHighLoad)
        }
        HealthMessageId::HealthTryInspectProcessMemory => {
            Some(HealthMessageId::HealthPromptInspectProcessMemory)
        }
        HealthMessageId::HealthTryReviewUnavailableChecks => {
            Some(HealthMessageId::HealthPromptReviewUnavailableChecks)
        }
        _ => None,
    }
}
