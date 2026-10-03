use super::*;
use crate::ui::CommandAssessmentSummaryModel;

fn turn_consent_model(next_label: Option<&str>) -> ApprovalPanelModel<'_> {
    ApprovalPanelModel {
        id: "req-2",
        kind: "tool request",
        risk: "medium",
        reason: None,
        subject: "tool Bash",
        preview_label: "Tool input",
        preview: "journalctl -u nginx -n 50",
        queue_position: 1,
        queue_total: 2,
        next_label,
        selected_action: ApprovalPanelAction::Approve,
        expanded: false,
        turn_consent: true,
        turn_extension: false,
        deny_always_trust: false,
        irrecoverable: false,
        hook_warnings: Vec::new(),
    }
}

/// 批量场景（turn_consent）才展示"本轮全部允许"；Standard 卡零变化
/// （SC7/SC8/V8/N8，issue #1773）。
#[test]
fn approval_panel_turn_consent_offers_batch_action_standard_does_not() {
    let renderer = RatatuiInlineRenderer::with_width(120);
    let turn = renderer
        .approval_panel_lines(turn_consent_model(Some("req-3 tool Bash")))
        .join("\n");
    assert!(turn.contains("Allow all this turn"), "{turn}");
    assert!(turn.contains("Always trust"), "{turn}");
    assert_rendered_width(&turn, 120);

    let mut standard_model = turn_consent_model(None);
    standard_model.turn_consent = false;
    standard_model.queue_total = 1;
    let standard = renderer.approval_panel_lines(standard_model).join("\n");
    assert!(!standard.contains("Allow all this turn"), "{standard}");
    assert!(standard.contains("Always trust"), "{standard}");
}

/// 80 列 EN：5 动作自动折成 2 行，无截断；面板宽度契约保持（D8/V8）。
#[test]
fn approval_panel_turn_consent_wraps_actions_at_narrow_width() {
    let renderer = RatatuiInlineRenderer::with_width(80);
    let lines = renderer.approval_panel_lines(turn_consent_model(None));
    let text = lines.join("\n");
    assert!(text.contains("Allow all this turn"), "{text}");
    assert!(text.contains("Always trust"), "{text}");
    assert!(text.contains("Deny"), "{text}");
    assert!(text.contains("Details"), "{text}");
    assert_rendered_width(&text, 80);
    // Deny/Details 折到第二行：不与首行动作同居一行。
    let deny_line = lines
        .iter()
        .find(|line| line.contains("Deny"))
        .expect("deny line");
    assert!(!deny_line.contains("Allow all this turn"), "{deny_line}");
}

/// 宽终端（≥120 列）单行容纳 5 动作（D8）。
#[test]
fn approval_panel_turn_consent_single_row_on_wide_terminal() {
    let renderer = RatatuiInlineRenderer::with_width(140);
    let lines = renderer.approval_panel_lines(turn_consent_model(None));
    let action_line = lines
        .iter()
        .find(|line| line.contains("Allow all this turn"))
        .expect("action line");
    assert!(action_line.contains("Details"), "{action_line}");
}

/// pack_action_rows 贪心打包：边界行为 + 单项超宽不截断（D8）。
#[test]
fn pack_action_rows_greedy_packing_contract() {
    use crate::ui::pack_action_rows;
    // EN TurnConsent 宽度：10/19/12/4/7，content 76 → [0,1,2] + [3,4]。
    assert_eq!(
        pack_action_rows(&[10, 19, 12, 4, 7], 76),
        vec![vec![0, 1, 2], vec![3, 4]]
    );
    // 宽终端：单行。
    assert_eq!(
        pack_action_rows(&[10, 19, 12, 4, 7], 116),
        vec![vec![0, 1, 2, 3, 4]]
    );
    // ZH TurnConsent 宽度：8/12/14/4/4，content 76 → 前 4 项 + 详情。
    assert_eq!(
        pack_action_rows(&[8, 12, 14, 4, 4], 76),
        vec![vec![0, 1, 2, 3], vec![4]]
    );
    // 单项超宽：独占一行，不截断。
    assert_eq!(pack_action_rows(&[100], 20), vec![vec![0]]);
    assert_eq!(pack_action_rows(&[], 76), Vec::<Vec<usize>>::new());
    // 极端 i18n 长文案（如未来 CJK 长 label）：折成三行仍保持顺序与
    // 完整性，高度与渲染同源，只增高不截断（评审 P2 边界回归）。
    assert_eq!(
        pack_action_rows(&[30, 30, 30, 30, 30], 76),
        vec![vec![0, 1], vec![2, 3], vec![4]]
    );
    assert_eq!(
        pack_action_rows(&[70, 70, 4], 76),
        vec![vec![0], vec![1], vec![2]]
    );
}

#[test]
fn approval_panel_renders_active_request_with_queue_summary() {
    let renderer = RatatuiInlineRenderer::with_width(140);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-1",
            kind: "tool request",
            risk: "medium",
            // Card policy (ARP): medium risk never carries a reason phrase.
            reason: None,
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "top -l 1 -o mem -n 20 | head -30",
            queue_position: 1,
            queue_total: 4,
            next_label: Some("req-2 tool Bash"),
            selected_action: ApprovalPanelAction::Approve,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("Approval req-1"), "{text}");
    assert!(text.contains("Run Bash command?"), "{text}");
    assert!(!text.contains("Reason:"), "{text}");
    assert!(!text.contains("\u{2514} Risk:"), "{text}");
    assert!(
        text.contains("$ top -l 1 -o mem -n 20 | head -30"),
        "{text}"
    );
    assert!(text.contains("Queue: 1/4 pending"), "{text}");
    assert!(text.contains("next req-2 tool Bash"), "{text}");
    assert!(text.contains("Allow once"), "{text}");
    assert!(text.contains("Deny"), "{text}");
    assert!(text.contains("Details"), "{text}");
    assert!(!text.contains("medium risk"), "{text}");
    assert!(!text.contains("Command:"), "{text}");
    assert!(!text.contains("Review tool request"), "{text}");
    assert!(!text.contains("/approve"), "{text}");
    assert!(!text.contains("Subject: tool Bash"), "{text}");
    assert!(!text.contains("Tool input"), "{text}");
    assert_rendered_width(&text, 140);
}

#[test]
fn approval_panel_high_risk_shows_reason_continuation_line() {
    let renderer = RatatuiInlineRenderer::with_width(140);
    let phrase = crate::ui::card_reason_phrase(
        "high",
        "privilege-escalation",
        crate::I18n::new(crate::Language::EnUs),
    )
    .expect("whitelisted high-risk phrase");
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-9",
            kind: "tool request",
            risk: "high",
            reason: Some(&phrase),
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "sudo rm -rf /data/legacy-cache",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Approve,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(
        text.contains("\u{2514} Risk: privilege escalation"),
        "{text}"
    );
    assert!(text.contains("$ sudo rm -rf /data/legacy-cache"), "{text}");
    assert!(!text.contains("privilege-escalation"), "{text}");
    assert!(!text.contains("Queue:"), "{text}");
    assert_rendered_width(&text, 140);
}

#[test]
fn approval_panel_high_risk_continuation_wraps_within_narrow_width() {
    // Review follow-up: full-card width contract for the continuation line,
    // not just the phrase-level budget (validation.md S2), in both catalogs
    // with the widest phrase of each language.
    for (language, code, expected_label) in [
        (
            crate::Language::EnUs,
            "service-or-container-control",
            "\u{2514} Risk:",
        ),
        (
            crate::Language::ZhCn,
            "interactive-editor",
            "\u{2514} 风险:",
        ),
    ] {
        let renderer = RatatuiInlineRenderer::with_width(60).with_language(language);
        let phrase = crate::ui::card_reason_phrase("high", code, crate::I18n::new(language))
            .expect("whitelisted high-risk phrase");
        assert!(
            crate::ui::agent_render::display_width(&phrase)
                <= crate::ui::CARD_REASON_PHRASE_MAX_WIDTH,
            "{language:?} phrase exceeds SDD budget: {phrase}"
        );
        let text = renderer
            .approval_panel_lines(ApprovalPanelModel {
                id: "req-9",
                kind: "tool request",
                risk: "high",
                reason: Some(&phrase),
                subject: "tool Bash",
                preview_label: "Tool input",
                preview: "kubectl delete pod payments --grace-period=0",
                queue_position: 1,
                queue_total: 1,
                next_label: None,
                selected_action: ApprovalPanelAction::Approve,
                expanded: false,
                turn_consent: false,
                turn_extension: false,
                deny_always_trust: false,
                irrecoverable: false,
                hook_warnings: Vec::new(),
            })
            .join("\n");

        assert!(text.contains(expected_label), "{language:?}: {text}");
        assert_rendered_width(&text, 60);
    }
}

#[test]
fn approval_panel_long_subject_never_hides_risk_badge() {
    // Review follow-up (#1786): unbounded custom/MCP tool names are
    // ellipsized so the risk badge and queue info keep their reserved width
    // even on a 40-column terminal.
    let renderer = RatatuiInlineRenderer::with_width(40);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-9",
            kind: "tool request",
            risk: "medium",
            reason: None,
            subject: "mcp__server__extremely_long_custom_tool_name",
            preview_label: "Tool input",
            preview: "echo hi",
            queue_position: 2,
            queue_total: 3,
            next_label: None,
            selected_action: ApprovalPanelAction::Approve,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("\u{2026} · medium risk"), "{text}");
    assert!(text.contains("queue 2/3"), "{text}");
    assert!(!text.contains("extremely_long_custom_tool_name"), "{text}");
    assert_rendered_width(&text, 40);
}

#[test]
fn approval_panel_unknown_risk_value_falls_back_to_localized_label() {
    // Review follow-up: values outside the closed legacy_risk() domain must
    // render a neutral localized badge instead of leaking the raw string
    // into a mixed-language metadata row.
    let renderer = RatatuiInlineRenderer::with_width(120).with_language(crate::Language::ZhCn);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-9",
            kind: "tool request",
            risk: "critical",
            reason: None,
            subject: "Bash",
            preview_label: "Tool 输入",
            preview: "echo hi",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Approve,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("Bash · 未知风险"), "{text}");
    assert!(!text.contains("critical"), "{text}");
    assert_rendered_width(&text, 120);
}

#[test]
fn approval_panel_uses_zh_labels_without_translating_command() {
    let renderer = RatatuiInlineRenderer::with_width(140).with_language(crate::Language::ZhCn);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-1",
            kind: "tool request",
            risk: "medium",
            reason: None,
            subject: "tool Bash",
            preview_label: "Tool 输入",
            preview: "top -l 1 -o mem -n 20 | head -30",
            queue_position: 1,
            queue_total: 2,
            next_label: Some("req-2 tool Bash"),
            selected_action: ApprovalPanelAction::Approve,
            expanded: true,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("审批 req-1"), "{text}");
    assert!(text.contains("运行 Bash 命令？"), "{text}");
    assert!(
        text.contains("$ top -l 1 -o mem -n 20 | head -30"),
        "{text}"
    );
    assert!(text.contains("队列: 1/2 待处理"), "{text}");
    assert!(text.contains("下一个 req-2 tool Bash"), "{text}");
    assert!(text.contains("允许一次"), "{text}");
    assert!(text.contains("始终信任"), "{text}");
    assert!(text.contains("拒绝"), "{text}");
    assert!(text.contains("详情"), "{text}");
    assert!(text.contains("按键:"), "{text}");
    assert!(text.contains("默认: 拒绝"), "{text}");
    assert_rendered_width(&text, 140);
}

#[test]
fn approval_panel_keeps_focus_visible_and_caps_long_preview() {
    let renderer = RatatuiInlineRenderer::with_width(82);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-1",
            kind: "tool request",
            risk: "medium",
            reason: None,
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "echo \"=== 系统内存概览 ===\" && vm_stat && echo \"\" && echo \"=== 内存占用 Top 10 进程 ===\" && ps aux -m | head -11 && echo \"=== CPU 占用 Top 10 进程 ===\" && ps aux -r | head -11 && echo \"=== AliEntSafe 进程 ===\" && ps aux | grep AliEntSafe",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Deny,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("> [ Deny ]"), "{text}");
    assert!(text.contains("..."), "{text}");
    assert!(!text.contains("Keys:"), "{text}");
    assert!(!text.contains("Left/Right select"), "{text}");
    assert_rendered_width(&text, 82);
}

#[test]
fn approval_panel_keeps_cjk_and_emoji_borders_aligned() {
    let renderer = RatatuiInlineRenderer::with_width(70);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-宽",
            kind: "tool request",
            risk: "medium",
            reason: None,
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "cat /tmp/cosh-shell-中文-smoke.txt && echo 🧪 系统负载分析完成 && printf 'done\\n'",
            queue_position: 1,
            queue_total: 3,
            next_label: Some("req-2 tool Bash"),
            selected_action: ApprovalPanelAction::Details,
            expanded: true,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("Approval req-宽"), "{text}");
    assert!(text.contains("$ cat /tmp/cosh-shell-中文"), "{text}");
    assert!(text.contains("> [ Details ]"), "{text}");
    assert!(text.contains("Queue: 1/3 pending"), "{text}");
    assert_rendered_width(&text, 70);
    assert_box_lines_aligned(&text, 70);
}

#[test]
fn approval_panel_renders_shell_command_request_as_compact_command() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-2",
            kind: "shell command request",
            risk: "high",
            reason: None,
            subject: "shell command",
            preview_label: "Command",
            preview: "touch /tmp/cosh-shell-fake-action-should-not-run",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Deny,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("Approval req-2"), "{text}");
    assert!(text.contains("Run shell command?"), "{text}");
    assert!(
        text.contains("$ touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{text}"
    );
    assert!(text.contains("> [ Deny ]"), "{text}");
    assert!(!text.contains("shell command request"), "{text}");
    assert!(!text.contains("high risk"), "{text}");
    assert!(!text.contains("Subject:"), "{text}");
    assert!(!text.contains("Command:"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn approval_panel_write_preserves_ratatui_styles_for_terminal_output() {
    let renderer = RatatuiInlineRenderer {
        width: 90,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let mut output = Vec::new();

    renderer
        .write_approval_panel(
            &mut output,
            ApprovalPanelModel {
                id: "req-1",
                kind: "tool request",
                risk: "high",
                reason: None,
                subject: "tool Bash",
                preview_label: "Tool input",
                preview: "pwd",
                queue_position: 1,
                queue_total: 1,
                next_label: None,
                selected_action: ApprovalPanelAction::Deny,
                expanded: false,
                turn_consent: false,
                turn_extension: false,
                deny_always_trust: false,
                irrecoverable: false,
                hook_warnings: Vec::new(),
            },
        )
        .expect("render approval panel");

    let text = String::from_utf8(output).expect("utf8 panel");
    let clean = strip_ansi_escape(&text);
    assert!(text.contains("\x1b["), "{text:?}");
    assert!(clean.contains("> [ Deny ]"), "{clean}");
    assert!(clean.contains("pwd"), "{clean}");
}

#[test]
fn approval_panel_styles_selected_actions_by_decision_kind() {
    let mut deny_output = Vec::new();
    RatatuiInlineRenderer {
        width: 90,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    }
    .write_approval_panel(
        &mut deny_output,
        ApprovalPanelModel {
            id: "req-1",
            kind: "tool request",
            risk: "medium",
            reason: None,
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "pwd",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Deny,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        },
    )
    .expect("render deny approval panel");
    let deny = String::from_utf8(deny_output).expect("utf8 deny panel");

    let mut details_output = Vec::new();
    RatatuiInlineRenderer {
        width: 90,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    }
    .write_approval_panel(
        &mut details_output,
        ApprovalPanelModel {
            id: "req-2",
            kind: "tool request",
            risk: "medium",
            reason: None,
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "pwd",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Details,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        },
    )
    .expect("render details approval panel");
    let details = String::from_utf8(details_output).expect("utf8 details panel");

    assert!(deny.contains("\x1b[0;1;97;41m> [ Deny ]"), "{deny:?}");
    assert!(!deny.contains("\x1b[0;1;97;42m> [ Deny ]"), "{deny:?}");
    assert!(
        details.contains("\x1b[0;1;97;44m> [ Details ]"),
        "{details:?}"
    );
}

#[test]
fn plain_approval_panel_keeps_queue_before_actions() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let lines = renderer.approval_panel_lines(ApprovalPanelModel {
        id: "req-1",
        kind: "tool request",
        risk: "medium",
        reason: None,
        subject: "tool Bash",
        preview_label: "Tool input",
        preview: "git status",
        queue_position: 1,
        queue_total: 2,
        next_label: Some("req-2 shell command"),
        selected_action: ApprovalPanelAction::Approve,
        expanded: false,
        turn_consent: false,
        turn_extension: false,
        deny_always_trust: false,
        irrecoverable: false,
        hook_warnings: Vec::new(),
    });
    let text = lines.join("\n");

    assert!(text.contains("Approval required"), "{text}");
    assert!(text.contains("Queue: 1/2 pending"), "{text}");
    assert!(text.contains("Run Bash command?"), "{text}");
    assert!(text.contains("$ git status"), "{text}");
    assert!(text.contains("next req-2 shell command"), "{text}");
    assert!(
        text.contains("[Allow once]  Always trust  Deny  Details"),
        "{text}"
    );
    assert!(
        line_index(&lines, "Queue: 1/2 pending; next req-2 shell command")
            < line_index(&lines, "[Allow once]  Always trust  Deny  Details"),
        "{text}"
    );
    assert!(!text.contains("medium risk"), "{text}");
    assert!(!text.contains("Command:"), "{text}");
    assert!(!text.contains("Review tool request"), "{text}");
}

#[test]
fn approval_receipt_panel_renders_auditable_decision() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let text = renderer
        .approval_receipt_panel_lines(ApprovalReceiptPanelModel {
            title: "Denied",
            negative: true,
            id: "req-1",
            kind: "Bash tool",
            decision: "denied by user",
            subject: "tool shell",
            preview: "git status",
            message: "No command ran.",
        })
        .join("\n");

    assert!(text.contains("Denied req-1"), "{text}");
    assert!(text.contains("Command: git status"), "{text}");
    assert!(text.contains("No command ran."), "{text}");
    assert!(!text.contains("Bash tool - denied by user"), "{text}");
    assert!(!text.contains("Subject:"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn approval_receipt_panel_uses_zh_fallback_labels() {
    let renderer = RatatuiInlineRenderer::with_width(100).with_language(crate::Language::ZhCn);
    let shell_text = renderer
        .approval_receipt_panel_lines(ApprovalReceiptPanelModel {
            title: "已拒绝",
            negative: true,
            id: "req-1",
            kind: "shell 命令请求",
            decision: "已拒绝",
            subject: "shell command",
            preview: "git status",
            message: "命令未运行。",
        })
        .join("\n");
    let preview_text = renderer
        .approval_receipt_panel_lines(ApprovalReceiptPanelModel {
            title: "已拒绝",
            negative: true,
            id: "req-2",
            kind: "tool 请求",
            decision: "已拒绝",
            subject: "tool Read",
            preview: r#"{"file_path":"Cargo.toml"}"#,
            message: "Tool 未运行。",
        })
        .join("\n");

    assert!(shell_text.contains("命令: git status"), "{shell_text}");
    assert!(
        preview_text.contains(r#"预览: {"file_path":"Cargo.toml"}"#),
        "{preview_text}"
    );
    assert!(!shell_text.contains("Command:"), "{shell_text}");
    assert!(!preview_text.contains("Preview:"), "{preview_text}");
}

#[test]
fn approval_receipt_panel_uses_negative_state_not_localized_title_for_style() {
    let renderer = RatatuiInlineRenderer {
        width: 100,
        plain: false,
        styled: true,
        language: crate::Language::ZhCn,
    };
    let mut output = Vec::new();

    renderer
        .write_approval_receipt_panel(
            &mut output,
            ApprovalReceiptPanelModel {
                title: "已拒绝",
                negative: true,
                id: "req-1",
                kind: "shell 命令请求",
                decision: "已拒绝",
                subject: "shell command",
                preview: "git status",
                message: "命令未运行。",
            },
        )
        .expect("render styled zh approval receipt");

    let text = String::from_utf8(output).expect("utf8 receipt");
    let clean = strip_ansi_escape(&text);
    assert!(text.contains("\x1b[0;31m"), "{text:?}");
    assert!(clean.contains("已拒绝 req-1"), "{clean}");
    assert!(clean.contains("命令: git status"), "{clean}");
}

#[test]
fn approval_receipt_panel_can_render_compact_bash_approval() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let text = renderer
        .approval_receipt_panel_lines(ApprovalReceiptPanelModel {
            title: "Approved",
            negative: false,
            id: "req-1",
            kind: "",
            decision: "",
            subject: "tool Bash",
            preview: "",
            message: "",
        })
        .join("\n");

    assert!(text.contains("Approved req-1"), "{text}");
    assert!(!text.contains("Bash tool - approved"), "{text}");
    assert!(!text.contains("Command:"), "{text}");
    assert!(!text.contains("Running command"), "{text}");
    assert!(!text.contains('┌'), "{text}");
    assert!(!text.contains('└'), "{text}");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn approval_receipt_panel_wraps_long_command_and_message() {
    let renderer = RatatuiInlineRenderer::with_width(62);
    let text = renderer
        .approval_receipt_panel_lines(ApprovalReceiptPanelModel {
            title: "Denied",
            negative: true,
            id: "req-9",
            kind: "shell command request",
            decision: "denied",
            subject: "shell command",
            preview: "touch /tmp/cosh-shell-fake-action-should-not-run && echo should-not-run",
            message: "No command ran; the shell prompt stays available for the next user command.",
        })
        .join("\n");

    assert!(text.contains("Denied req-9"), "{text}");
    assert!(
        text.contains("Command: touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{text}"
    );
    assert!(text.contains("         && echo should-not-run"), "{text}");
    assert!(
        text.contains("No command ran; the shell prompt stays available for the"),
        "{text}"
    );
    assert!(text.contains("next user command."), "{text}");
    assert_rendered_width(&text, 62);
}

#[test]
fn approval_receipt_panel_keeps_cjk_and_emoji_borders_aligned() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let text = renderer
        .approval_receipt_panel_lines(ApprovalReceiptPanelModel {
            title: "Denied",
            negative: true,
            id: "req-宽",
            kind: "shell command request",
            decision: "denied",
            subject: "shell command",
            preview: "cat /tmp/cosh-shell-中文-smoke.txt && echo 🧪 should-not-run",
            message: "No command ran; shell prompt stays available.",
        })
        .join("\n");

    assert!(text.contains("Denied req-宽"), "{text}");
    assert!(text.contains("Command: cat"), "{text}");
    assert!(text.contains("中文-smoke.txt"), "{text}");
    assert!(text.contains("No command ran"), "{text}");
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

#[test]
fn plain_approval_receipt_panel_keeps_cancel_text() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let text = renderer
        .approval_receipt_panel_lines(ApprovalReceiptPanelModel {
            title: "Cancelled",
            negative: true,
            id: "req-2",
            kind: "shell command request",
            decision: "cancelled by user",
            subject: "shell command",
            preview: "touch /tmp/nope",
            message: "No command ran.",
        })
        .join("\n");

    assert!(text.contains("Cancelled req-2"), "{text}");
    assert!(text.contains("Command: touch /tmp/nope"), "{text}");
    assert!(text.contains("No command ran."), "{text}");
    assert!(
        !text.contains("shell command request - cancelled by user"),
        "{text}"
    );
    assert!(!text.contains('╭'), "{text}");
}

#[test]
fn plain_approval_receipt_panel_wraps_long_command() {
    let renderer = RatatuiInlineRenderer::plain_with_width(50);
    let text = renderer
        .approval_receipt_panel_lines(ApprovalReceiptPanelModel {
            title: "Denied",
            negative: true,
            id: "req-10",
            kind: "shell command request",
            decision: "denied",
            subject: "shell command",
            preview: "touch /tmp/cosh-shell-fake-action-should-not-run && echo should-not-run",
            message: "No command ran; the shell prompt stays available.",
        })
        .join("\n");

    assert!(text.contains("Denied req-10"), "{text}");
    assert!(text.contains("Command: touch"), "{text}");
    assert!(
        text.contains("         /tmp/cosh-shell-fake-action-should-no"),
        "{text}"
    );
    assert!(
        text.contains("         t-run && echo should-not-run"),
        "{text}"
    );
    assert!(
        text.contains("No command ran; the shell prompt stays"),
        "{text}"
    );
    assert!(text.contains("available."), "{text}");
    assert!(!text.contains('┌'), "{text}");
    assert_rendered_width(&text, 50);
}

#[test]
fn approval_details_panel_renders_structured_request_context() {
    let renderer = RatatuiInlineRenderer::with_width(70);
    let text = renderer
        .approval_details_panel_lines(ApprovalDetailsPanelModel {
            id: "req-7",
            run_id: "run-12",
            source: "agent",
            kind: "tool request",
            status: "pending",
            risk: "high",
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "echo system && ps aux -m | head -11 && echo done",
            request_id: None,
            tool_use_id: None,
            execution_path: Some("foreground_shell_pty"),
            command_block_id: Some("cmd-7"),
            redaction_status: Some("ref_only"),
            assessment: Some(CommandAssessmentSummaryModel {
                impact: "medium",
                execution: "ask-user",
                confidence: "medium",
                primary_reason: "diagnostic-pipeline-heuristic",
                reason_trace: "diagnostic-pipeline-heuristic,pipeline-not-auto-executable",
                auto_allow: None,
                output_stability: "stable-snapshot",
                output_exposure: "may-contain-command-line",
            }),
            audit_ref: None,
        })
        .join("\n");

    assert!(text.contains("Approval details req-7"), "{text}");
    assert!(text.contains("tool request  pending  high risk"), "{text}");
    assert!(text.contains("Source: agent"), "{text}");
    assert!(text.contains("Run: run-12"), "{text}");
    assert!(text.contains("Execution: foreground_shell_pty"), "{text}");
    assert!(text.contains("Command block: cmd-7"), "{text}");
    assert!(text.contains("Redaction: ref_only"), "{text}");
    assert!(
        text.contains("Assessment: impact medium; decision ask-user; confidence medium"),
        "{text}"
    );
    assert!(
        text.contains("Reason: diagnostic-pipeline-heuristic"),
        "{text}"
    );
    assert!(text.contains("Default: deny"), "{text}");
    assert!(text.contains("Request: Bash command"), "{text}");
    assert!(text.contains("Command:"), "{text}");
    assert!(text.contains("ps aux -m"), "{text}");
    assert!(text.contains("Policy: user approval is required"), "{text}");
    assert!(!text.contains("Subject: tool Bash"), "{text}");
    assert!(!text.contains("Tool input"), "{text}");
    assert!(!text.contains("Approval details\nid:"), "{text}");
    assert_rendered_width(&text, 70);
}

#[test]
fn approval_details_panel_uses_zh_catalog_labels() {
    let renderer = RatatuiInlineRenderer::with_width(70).with_language(crate::Language::ZhCn);
    let text = renderer
        .approval_details_panel_lines(ApprovalDetailsPanelModel {
            id: "req-7",
            run_id: "run-12",
            source: "agent",
            kind: "tool request",
            status: "pending",
            risk: "high",
            subject: "tool Bash",
            preview_label: "Tool 输入",
            preview: "echo system && ps aux -m | head -11 && echo done",
            request_id: None,
            tool_use_id: None,
            execution_path: Some("foreground_shell_pty"),
            command_block_id: Some("cmd-7"),
            redaction_status: Some("ref_only"),
            assessment: Some(CommandAssessmentSummaryModel {
                impact: "medium",
                execution: "ask-user",
                confidence: "medium",
                primary_reason: "diagnostic-pipeline-heuristic",
                reason_trace: "diagnostic-pipeline-heuristic,pipeline-not-auto-executable",
                auto_allow: None,
                output_stability: "stable-snapshot",
                output_exposure: "may-contain-command-line",
            }),
            audit_ref: None,
        })
        .join("\n");

    assert!(text.contains("审批详情 req-7"), "{text}");
    assert!(text.contains("风险 high"), "{text}");
    assert!(text.contains("来源: agent"), "{text}");
    assert!(text.contains("运行: run-12"), "{text}");
    assert!(text.contains("执行: foreground_shell_pty"), "{text}");
    assert!(text.contains("命令块: cmd-7"), "{text}");
    assert!(text.contains("脱敏: ref_only"), "{text}");
    assert!(
        text.contains("评估: 影响 medium；决策 ask-user；置信度 medium"),
        "{text}"
    );
    assert!(
        text.contains("原因: diagnostic-pipeline-heuristic"),
        "{text}"
    );
    assert!(text.contains("默认: 拒绝"), "{text}");
    assert!(text.contains("请求: Bash 命令"), "{text}");
    assert!(text.contains("命令:"), "{text}");
    assert!(
        text.contains("策略: 可执行 tool 请求必须先经过用户审批。"),
        "{text}"
    );
    assert!(!text.contains("Approval details"), "{text}");
    assert!(!text.contains("Tool input"), "{text}");
}

#[test]
fn approval_details_panel_keeps_cjk_and_emoji_borders_aligned() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let text = renderer
        .approval_details_panel_lines(ApprovalDetailsPanelModel {
            id: "req-宽",
            run_id: "run-中文-1",
            source: "agent",
            kind: "tool request",
            status: "pending",
            risk: "high",
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "cat /tmp/cosh-shell-中文-smoke.txt && echo 🧪 approval details",
            request_id: None,
            tool_use_id: None,
            execution_path: None,
            command_block_id: None,
            redaction_status: None,
            assessment: None,
            audit_ref: None,
        })
        .join("\n");

    assert!(text.contains("Approval details req-宽"), "{text}");
    assert!(text.contains("run-中文-1"), "{text}");
    assert!(text.contains("中文-smoke.txt"), "{text}");
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

#[test]
fn approval_details_panel_renders_audit_reference_inside_panel() {
    let renderer = RatatuiInlineRenderer::with_width(70);
    let text = renderer
        .approval_details_panel_lines(ApprovalDetailsPanelModel {
            id: "req-7",
            run_id: "run-12",
            source: "agent",
            kind: "tool request",
            status: "approved",
            risk: "medium",
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "ls -la",
            request_id: Some("ctrl-7"),
            tool_use_id: Some("toolu-7"),
            execution_path: Some("foreground_shell_pty"),
            command_block_id: Some("cmd-7"),
            redaction_status: Some("ref_only"),
            assessment: None,
            audit_ref: Some("audit-event-1"),
        })
        .join("\n");

    assert!(text.contains("audit_ref: audit-event-1"), "{text}");
    // The reference must live inside the panel body, never after the closing border.
    let closing_border = text.lines().next_back().unwrap_or_default();
    assert!(closing_border.contains('└'), "{text}");
    assert!(!closing_border.contains("audit_ref"), "{text}");
    assert!(text.contains("ls -la"), "{text}");
    assert!(text.contains("Policy: user approval is required"), "{text}");
    assert_rendered_width(&text, 70);
    assert_box_lines_aligned(&text, 70);
}

/// Real audit event ids are UUIDs, so `audit_ref: <uuid>` is 47 columns wide.
const UUID_AUDIT_REF: &str = "cd8a7e91-a95d-4c4f-bb0f-646f4b154310";

/// Strips borders and padding so a wrapped reference can be matched as one value.
fn panel_content_without_layout(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace() && !"│┌┐└┘─".contains(*ch))
        .collect()
}

#[test]
fn approval_details_panel_wraps_audit_reference_at_minimum_width() {
    // The 40-column minimum panel is narrower than `audit_ref: <uuid>`; the id
    // must wrap instead of being clipped, or it cannot be traced in the audit log.
    let renderer = RatatuiInlineRenderer::with_width(40);
    let model = ApprovalDetailsPanelModel {
        id: "req-7",
        run_id: "run-12",
        source: "agent",
        kind: "tool request",
        status: "approved",
        risk: "medium",
        subject: "tool Bash",
        preview_label: "Tool input",
        preview: "ls -la",
        request_id: None,
        tool_use_id: None,
        execution_path: None,
        command_block_id: None,
        redaction_status: None,
        assessment: None,
        audit_ref: Some(UUID_AUDIT_REF),
    };
    let with_ref = renderer.approval_details_panel_lines(model.clone());
    let without_ref = renderer.approval_details_panel_lines(ApprovalDetailsPanelModel {
        audit_ref: None,
        ..model
    });
    let text = with_ref.join("\n");

    assert!(
        panel_content_without_layout(&text).contains(&format!("audit_ref:{UUID_AUDIT_REF}")),
        "{text}"
    );
    assert!(!text.contains(" ..."), "{text}");
    // The wrapped rows must be budgeted, not stolen from the rows below.
    assert_eq!(with_ref.len(), without_ref.len() + 2, "{text}");
    assert!(text.contains("Policy:"), "{text}");
    assert_rendered_width(&text, 40);
    assert_box_lines_aligned(&text, 40);
}

#[test]
fn approval_journal_panel_wraps_audit_reference_at_minimum_width() {
    // The journal body is a wrapping Paragraph, so a reference that spans two
    // rows must be counted twice in the panel height or the trailing entry rows
    // (actor, preview hash, subject, preview) get clipped.
    let renderer = RatatuiInlineRenderer::with_width(40);
    let mut entries = audit_ref_journal_entries();
    entries[0].audit_ref = Some(UUID_AUDIT_REF);
    let with_ref =
        renderer.approval_journal_panel_lines(ApprovalJournalPanelModel { entries: &entries });
    entries[0].audit_ref = None;
    let without_ref =
        renderer.approval_journal_panel_lines(ApprovalJournalPanelModel { entries: &entries });
    let text = with_ref.join("\n");

    assert!(
        panel_content_without_layout(&text).contains(&format!("audit_ref:{UUID_AUDIT_REF}")),
        "{text}"
    );
    assert!(!text.contains(" ..."), "{text}");
    assert_eq!(with_ref.len(), without_ref.len() + 2, "{text}");
    assert_rendered_width(&text, 40);
    assert_box_lines_aligned(&text, 40);
}

#[test]
fn approval_details_panel_keeps_borders_aligned_with_uuid_audit_reference() {
    // Real audit event ids are UUIDs; the widest realistic reference must still
    // fit the narrow-terminal panel without breaking the border.
    let renderer = RatatuiInlineRenderer::with_width(54).with_language(crate::Language::ZhCn);
    let text = renderer
        .approval_details_panel_lines(ApprovalDetailsPanelModel {
            id: "req-宽",
            run_id: "run-中文-1",
            source: "agent",
            kind: "tool request",
            status: "approved",
            risk: "medium",
            subject: "tool Bash",
            preview_label: "Tool 输入",
            preview: "cat /tmp/cosh-shell-中文-smoke.txt && echo 🧪",
            request_id: None,
            tool_use_id: None,
            execution_path: None,
            command_block_id: None,
            redaction_status: None,
            assessment: None,
            audit_ref: Some("cd8a7e91-a95d-4c4f-bb0f-646f4b154310"),
        })
        .join("\n");

    assert!(
        text.contains("audit_ref: cd8a7e91-a95d-4c4f-bb0f-646f4b154310"),
        "{text}"
    );
    assert!(text.contains("中文-smoke.txt"), "{text}");
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

#[test]
fn styled_approval_details_write_keeps_audit_reference_inside_panel() {
    let renderer = RatatuiInlineRenderer {
        width: 70,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let mut output = Vec::new();

    renderer
        .write_approval_details_panel(
            &mut output,
            ApprovalDetailsPanelModel {
                id: "req-7",
                run_id: "run-12",
                source: "agent",
                kind: "tool request",
                status: "approved",
                risk: "medium",
                subject: "tool Bash",
                preview_label: "Tool input",
                preview: "ls -la",
                request_id: None,
                tool_use_id: None,
                execution_path: None,
                command_block_id: None,
                redaction_status: None,
                assessment: None,
                audit_ref: Some("audit-event-1"),
            },
        )
        .expect("render approval details panel");

    let text = String::from_utf8(output).expect("utf8 panel");
    let clean = strip_ansi_escape(&text);
    assert!(text.contains("\x1b["), "{text:?}");
    assert!(clean.contains("audit_ref: audit-event-1"), "{clean}");
    let closing_border = clean.trim_end().lines().next_back().unwrap_or_default();
    assert!(closing_border.contains('└'), "{clean}");
    assert!(!closing_border.contains("audit_ref"), "{clean}");
}

#[test]
fn approval_details_panel_omits_audit_reference_when_absent() {
    let renderer = RatatuiInlineRenderer::with_width(70);
    let model = ApprovalDetailsPanelModel {
        id: "req-7",
        run_id: "run-12",
        source: "agent",
        kind: "tool request",
        status: "approved",
        risk: "medium",
        subject: "tool Bash",
        preview_label: "Tool input",
        preview: "ls -la",
        request_id: Some("ctrl-7"),
        tool_use_id: Some("toolu-7"),
        execution_path: Some("foreground_shell_pty"),
        command_block_id: Some("cmd-7"),
        redaction_status: Some("ref_only"),
        assessment: None,
        audit_ref: None,
    };
    let without_ref = renderer.approval_details_panel_lines(model.clone());
    let with_ref = renderer.approval_details_panel_lines(ApprovalDetailsPanelModel {
        audit_ref: Some("audit-event-1"),
        ..model
    });

    let text = without_ref.join("\n");
    assert!(!text.contains("audit_ref"), "{text}");
    // A missing reference must not leave a blank placeholder row behind.
    assert_eq!(without_ref.len() + 1, with_ref.len(), "{text}");
    assert_rendered_width(&text, 70);
    assert_box_lines_aligned(&text, 70);
}

#[test]
fn plain_approval_details_panel_keeps_audit_reference() {
    let renderer = RatatuiInlineRenderer::plain_with_width(70);
    let model = ApprovalDetailsPanelModel {
        id: "req-7",
        run_id: "run-12",
        source: "agent",
        kind: "tool request",
        status: "approved",
        risk: "medium",
        subject: "tool Bash",
        preview_label: "Tool input",
        preview: "ls -la",
        request_id: None,
        tool_use_id: None,
        execution_path: None,
        command_block_id: None,
        redaction_status: None,
        assessment: None,
        audit_ref: Some("audit-event-1"),
    };
    let text = renderer
        .approval_details_panel_lines(model.clone())
        .join("\n");

    assert!(text.contains("audit_ref: audit-event-1"), "{text}");
    assert!(!text.contains('┌'), "{text}");

    let without_ref = renderer
        .approval_details_panel_lines(ApprovalDetailsPanelModel {
            audit_ref: None,
            ..model
        })
        .join("\n");
    assert!(!without_ref.contains("audit_ref"), "{without_ref}");
}

#[test]
fn approval_journal_panel_renders_decision_history() {
    let renderer = RatatuiInlineRenderer::with_width(88);
    let entries = vec![
        ApprovalJournalEntryModel {
            id: "req-1",
            run_id: "run-1",
            source: "agent",
            decision: "approved",
            kind: "tool request",
            risk: "medium",
            subject: "tool shell",
            preview: "git status",
            preview_hash: "fnv1a64:test0001",
            request_id: Some("ctrl-1"),
            tool_use_id: Some("toolu-1"),
            actor: "agent-auto",
            execution_path: Some("foreground_shell_pty"),
            command_block_id: Some("cmd-1"),
            redaction_status: Some("ref_only"),
            assessment: Some(CommandAssessmentSummaryModel {
                impact: "low",
                execution: "auto-allow",
                confidence: "high",
                primary_reason: "bounded-readonly",
                reason_trace: "bounded-readonly",
                auto_allow: Some("bounded-readonly"),
                output_stability: "stable-snapshot",
                output_exposure: "normal",
            }),
            audit_ref: None,
        },
        ApprovalJournalEntryModel {
            id: "req-2",
            run_id: "run-1",
            source: "agent",
            decision: "denied",
            kind: "shell command request",
            risk: "high",
            subject: "shell command",
            preview: "touch /tmp/cosh-shell-fake-action-should-not-run",
            preview_hash: "fnv1a64:test0002",
            request_id: None,
            tool_use_id: None,
            actor: "user",
            execution_path: Some("not_executed_denied"),
            command_block_id: None,
            redaction_status: None,
            assessment: None,
            audit_ref: None,
        },
    ];
    let text = renderer
        .approval_journal_panel_lines(ApprovalJournalPanelModel { entries: &entries })
        .join("\n");

    assert!(text.contains("Approval journal 2 decisions"), "{text}");
    assert!(text.contains("req-1  approved  tool request"), "{text}");
    assert!(text.contains("Source: agent  Run: run-1"), "{text}");
    assert!(text.contains("Execution: foreground_shell_pty"), "{text}");
    assert!(text.contains("Command block: cmd-1"), "{text}");
    assert!(text.contains("Redaction: ref_only"), "{text}");
    assert!(
        text.contains("Assessment: impact low; decision auto-allow; confidence high"),
        "{text}"
    );
    assert!(text.contains("Reason: bounded-readonly"), "{text}");
    assert!(text.contains("Actor: agent-auto"), "{text}");
    assert!(text.contains("Command: git status"), "{text}");
    assert!(
        text.contains("req-2  denied  shell command request"),
        "{text}"
    );
    assert!(
        text.contains("touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{text}"
    );
    assert!(!text.contains("run:"), "{text}");
    assert_rendered_width(&text, 88);
}

#[test]
fn approval_journal_panel_uses_zh_catalog_labels() {
    let renderer = RatatuiInlineRenderer::with_width(88).with_language(crate::Language::ZhCn);
    let entries = vec![ApprovalJournalEntryModel {
        id: "req-1",
        run_id: "run-1",
        source: "agent",
        decision: "approved",
        kind: "tool request",
        risk: "medium",
        subject: "tool shell",
        preview: "git status",
        preview_hash: "fnv1a64:test0001",
        request_id: Some("ctrl-1"),
        tool_use_id: Some("toolu-1"),
        actor: "agent-auto",
        execution_path: Some("foreground_shell_pty"),
        command_block_id: Some("cmd-1"),
        redaction_status: Some("ref_only"),
        assessment: None,
        audit_ref: None,
    }];
    let text = renderer
        .approval_journal_panel_lines(ApprovalJournalPanelModel { entries: &entries })
        .join("\n");

    assert!(text.contains("审批记录 1 条决策"), "{text}");
    assert!(text.contains("风险 medium"), "{text}");
    assert!(text.contains("来源: agent"), "{text}");
    assert!(text.contains("运行: run-1"), "{text}");
    assert!(text.contains("执行: foreground_shell_pty"), "{text}");
    assert!(text.contains("命令块: cmd-1"), "{text}");
    assert!(text.contains("脱敏: ref_only"), "{text}");
    assert!(text.contains("Provider 请求: ctrl-1"), "{text}");
    assert!(text.contains("Tool 使用: toolu-1"), "{text}");
    assert!(text.contains("执行者: agent-auto"), "{text}");
    assert!(text.contains("预览哈希: fnv1a64:test0001"), "{text}");
    assert!(text.contains("对象: tool shell"), "{text}");
    assert!(text.contains("命令: git status"), "{text}");
    assert!(!text.contains("Approval journal"), "{text}");
    assert!(!text.contains("Command block:"), "{text}");
}

#[test]
fn approval_journal_panel_keeps_cjk_and_emoji_borders_aligned() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let entries = vec![ApprovalJournalEntryModel {
        id: "req-宽",
        run_id: "run-中文-1",
        source: "agent",
        decision: "denied",
        kind: "shell command request",
        risk: "high",
        subject: "shell command",
        preview: "cat /tmp/cosh-shell-中文-smoke.txt && echo 🧪 should-not-run",
        preview_hash: "fnv1a64:test0003",
        request_id: None,
        tool_use_id: None,
        actor: "user",
        execution_path: Some("not_executed_denied"),
        command_block_id: None,
        redaction_status: None,
        assessment: None,
        audit_ref: None,
    }];
    let text = renderer
        .approval_journal_panel_lines(ApprovalJournalPanelModel { entries: &entries })
        .join("\n");

    assert!(text.contains("Approval journal 1 decisions"), "{text}");
    assert!(text.contains("req-宽"), "{text}");
    assert!(text.contains("run-中文-1"), "{text}");
    assert!(text.contains("中文-smoke.txt"), "{text}");
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

fn audit_ref_journal_entries() -> Vec<ApprovalJournalEntryModel<'static>> {
    vec![
        ApprovalJournalEntryModel {
            id: "req-1",
            run_id: "run-1",
            source: "agent",
            decision: "approved",
            kind: "tool request",
            risk: "medium",
            subject: "tool shell",
            preview: "git status",
            preview_hash: "fnv1a64:test0001",
            request_id: Some("ctrl-1"),
            tool_use_id: Some("toolu-1"),
            actor: "agent-auto",
            execution_path: Some("foreground_shell_pty"),
            command_block_id: Some("cmd-1"),
            redaction_status: Some("ref_only"),
            assessment: None,
            audit_ref: Some("audit-event-1"),
        },
        ApprovalJournalEntryModel {
            id: "req-2",
            run_id: "run-1",
            source: "agent",
            decision: "denied",
            kind: "shell command request",
            risk: "high",
            subject: "shell command",
            preview: "rm -rf /tmp/cosh-shell-should-not-run",
            preview_hash: "fnv1a64:test0002",
            request_id: None,
            tool_use_id: None,
            actor: "user",
            execution_path: Some("not_executed_denied"),
            command_block_id: None,
            redaction_status: None,
            assessment: None,
            audit_ref: None,
        },
    ]
}

/// Splits the rendered journal at the `req-2` header so each entry's rows can be
/// asserted independently.
fn split_journal_entries(text: &str) -> (String, String) {
    let lines = text.lines().collect::<Vec<_>>();
    let boundary = lines
        .iter()
        .position(|line| line.contains("req-2"))
        .expect("req-2 entry header");
    (lines[..boundary].join("\n"), lines[boundary..].join("\n"))
}

#[test]
fn approval_journal_panel_scopes_audit_reference_to_owning_entry() {
    let renderer = RatatuiInlineRenderer::with_width(88);
    let entries = audit_ref_journal_entries();
    let lines =
        renderer.approval_journal_panel_lines(ApprovalJournalPanelModel { entries: &entries });
    let text = lines.join("\n");

    assert_eq!(text.matches("audit_ref").count(), 1, "{text}");
    let (first, second) = split_journal_entries(&text);
    assert!(first.contains("audit_ref: audit-event-1"), "{first}");
    assert!(!second.contains("audit_ref"), "{second}");
    // Nothing may trail the closing border of the panel.
    assert!(
        lines.last().is_some_and(|line| line.contains('└')),
        "{text}"
    );
    assert!(
        second.contains("rm -rf /tmp/cosh-shell-should-not-run"),
        "{second}"
    );
    assert_rendered_width(&text, 88);
    assert_box_lines_aligned(&text, 88);
}

#[test]
fn styled_approval_journal_write_scopes_audit_reference_to_owning_entry() {
    let renderer = RatatuiInlineRenderer {
        width: 88,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let entries = audit_ref_journal_entries();
    let mut output = Vec::new();

    renderer
        .write_approval_journal_panel(&mut output, ApprovalJournalPanelModel { entries: &entries })
        .expect("render approval journal panel");

    let text = String::from_utf8(output).expect("utf8 panel");
    let clean = strip_ansi_escape(&text);
    assert!(text.contains("\x1b["), "{text:?}");
    assert_eq!(clean.matches("audit_ref").count(), 1, "{clean}");
    let (first, second) = split_journal_entries(clean.trim_end());
    assert!(first.contains("audit_ref: audit-event-1"), "{first}");
    assert!(!second.contains("audit_ref"), "{second}");
    assert!(
        second
            .trim_end()
            .lines()
            .next_back()
            .is_some_and(|line| line.contains('└')),
        "{clean}"
    );
}

#[test]
fn plain_approval_journal_panel_scopes_audit_reference_to_owning_entry() {
    let renderer = RatatuiInlineRenderer::plain_with_width(88);
    let entries = audit_ref_journal_entries();
    let text = renderer
        .approval_journal_panel_lines(ApprovalJournalPanelModel { entries: &entries })
        .join("\n");

    assert_eq!(text.matches("audit_ref").count(), 1, "{text}");
    let (first, second) = split_journal_entries(&text);
    assert!(first.contains("audit_ref: audit-event-1"), "{first}");
    assert!(!second.contains("audit_ref"), "{second}");
    assert!(!text.contains('┌'), "{text}");
}

#[test]
fn plain_approval_journal_panel_keeps_decision_history() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let entries = vec![ApprovalJournalEntryModel {
        id: "req-1",
        run_id: "run-1",
        source: "agent",
        decision: "cancelled",
        kind: "tool request",
        risk: "medium",
        subject: "tool shell",
        preview: "git status",
        preview_hash: "fnv1a64:test0004",
        request_id: None,
        tool_use_id: None,
        actor: "user",
        execution_path: Some("not_executed_cancelled"),
        command_block_id: None,
        redaction_status: None,
        assessment: None,
        audit_ref: None,
    }];
    let text = renderer
        .approval_journal_panel_lines(ApprovalJournalPanelModel { entries: &entries })
        .join("\n");

    assert!(text.contains("Approval journal - 1 decisions"), "{text}");
    assert!(text.contains("req-1 cancelled - tool request"), "{text}");
    assert!(text.contains("Execution: not_executed_cancelled"), "{text}");
    assert!(text.contains("Command: git status"), "{text}");
    assert!(!text.contains('┌'), "{text}");
}

/// Irrecoverable command card (#2064): warning line renders between the
/// risk metadata and the command, and the high-risk action set never
/// offers AlwaysTrust.
#[test]
fn approval_panel_irrecoverable_command_warns_and_hides_always_trust() {
    let renderer = RatatuiInlineRenderer::with_width(120);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-1",
            kind: "tool request",
            risk: "high",
            reason: Some("system reboot/halt"),
            subject: "tool Bash",
            preview_label: "Command",
            preview: "reboot",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Approve,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: true,
            irrecoverable: true,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("irrecoverable"), "{text}");
    assert!(text.contains("SSH sessions drop"), "{text}");
    assert!(!text.contains("Always trust"), "{text}");
    assert!(text.contains("Allow once"), "{text}");
    assert!(text.contains("Deny"), "{text}");
}

/// Medium-risk cards keep today's shape: no warning line, AlwaysTrust
/// stays offered.
#[test]
fn approval_panel_medium_risk_card_keeps_always_trust_and_no_warning() {
    let renderer = RatatuiInlineRenderer::with_width(120);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-1",
            kind: "tool request",
            risk: "medium",
            reason: None,
            subject: "tool Bash",
            preview_label: "Command",
            preview: "npm test",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Approve,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: false,
            irrecoverable: false,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(!text.contains("irrecoverable"), "{text}");
    assert!(text.contains("Always trust"), "{text}");
}

/// The generic (non-command-heading) card shape — what a provider
/// control-permission request with subject "Bash" actually renders as at
/// runtime — must carry the same irrecoverable warning and high-risk
/// action set (#2064 acceptance regression: the warning was initially
/// wired only into the command-heading panel).
#[test]
fn approval_panel_generic_card_irrecoverable_warning_and_no_trust() {
    let renderer = RatatuiInlineRenderer::with_width(120);
    let text = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "req-1",
            kind: "tool request",
            risk: "high",
            reason: Some("system reboot/halt"),
            subject: "Bash",
            preview_label: "Command",
            preview: "$ reboot",
            queue_position: 1,
            queue_total: 1,
            next_label: None,
            selected_action: ApprovalPanelAction::Approve,
            expanded: false,
            turn_consent: false,
            turn_extension: false,
            deny_always_trust: true,
            irrecoverable: true,
            hook_warnings: Vec::new(),
        })
        .join("\n");

    assert!(text.contains("high risk"), "{text}");
    assert!(text.contains("irrecoverable"), "{text}");
    assert!(text.contains("SSH sessions drop"), "{text}");
    assert!(!text.contains("Always trust"), "{text}");
    assert!(text.contains("Allow once"), "{text}");
}
