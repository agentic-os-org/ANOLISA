use crate::ui::{
    ApprovalPanelAction, ApprovalPanelModel, NoticePanelModel, RatatuiInlineRenderer,
    ToolInvocationCardModel, ToolInvocationDensity, ToolInvocationTone,
};

#[test]
fn input_ownership_and_output_events_use_distinct_symbols() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);

    let mut agent = Vec::new();
    renderer
        .write_agent_response(&mut agent, "hello", None)
        .expect("agent card");
    let agent = String::from_utf8(agent).unwrap();
    assert!(agent.starts_with("Agent:"), "{agent}");
    assert!(!agent.starts_with("◆"), "{agent}");

    let mut slash = Vec::new();
    renderer
        .write_slash_notice_panel(
            &mut slash,
            NoticePanelModel {
                title: "Slash command",
                body: vec!["completed".to_string()],
                footer: None,
            },
        )
        .expect("slash card");
    assert!(String::from_utf8(slash)
        .unwrap()
        .starts_with("/ Slash command:"));

    let tool = renderer
        .tool_invocation_card_lines(ToolInvocationCardModel {
            title: "Read completed".to_string(),
            status: "success".to_string(),
            density: ToolInvocationDensity::Receipt,
            primary: "Cargo.toml".to_string(),
            result: "2 lines returned".to_string(),
            metrics: Vec::new(),
            action: None,
            debug_ref: None,
            tone: ToolInvocationTone::Success,
        })
        .join("\n");
    assert!(tool.starts_with("* Read completed"), "{tool}");

    let permission = renderer
        .approval_panel_lines(ApprovalPanelModel {
            id: "request-1",
            kind: "tool request",
            risk: "medium",
            reason: None,
            subject: "tool Bash",
            preview_label: "Tool input",
            preview: "id",
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
    assert!(
        permission.starts_with("! Approval required"),
        "{permission}"
    );

    let mut system = Vec::new();
    renderer
        .write_notice_panel(
            &mut system,
            NoticePanelModel {
                title: "System notice",
                body: vec!["ready".to_string()],
                footer: None,
            },
        )
        .expect("system card");
    assert!(String::from_utf8(system)
        .unwrap()
        .starts_with("· System notice:"));
}

#[test]
fn visible_symbols_in_notice_content_do_not_create_nested_cards() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let body = vec![
        "$ shell text".to_string(),
        "▶ direct-looking text".to_string(),
        "◆ agent-looking text".to_string(),
        "/ slash-looking text".to_string(),
        "* tool-looking text".to_string(),
        "! permission-looking text".to_string(),
        "· system-looking text".to_string(),
    ];
    let mut output = Vec::new();
    renderer
        .write_notice_panel(
            &mut output,
            NoticePanelModel {
                title: "System notice",
                body: body.clone(),
                footer: None,
            },
        )
        .expect("system card");
    let text = String::from_utf8(output).unwrap();

    assert!(text.starts_with("· System notice:"), "{text}");
    for line in body.into_iter().filter(|line| !line.starts_with("* ")) {
        assert!(text.contains(&format!("  {line}")), "{line}: {text}");
    }
    // The shared plain-text wrapper normalizes Markdown bullets for display;
    // the surrounding card identity remains System and no tool card appears.
    assert!(text.contains("  - tool-looking text"), "{text}");
    assert_eq!(text.lines().filter(|line| line.ends_with(':')).count(), 1);
}
