//! Contract tests for the `/hooks` status panel, which shares the
//! reference-panel visual contract with `/help`.

use super::{
    display_width, strip_ansi_escape, AgentHooksView, HookEntryView, HookEventGroup,
    HookStatusPanelModel, RatatuiInlineRenderer,
};

fn shell_entry(name: &str, detail: &str, disabled: bool) -> HookEntryView {
    HookEntryView {
        name: name.to_string(),
        detail: detail.to_string(),
        disabled,
    }
}

fn sample_model() -> HookStatusPanelModel<'static> {
    HookStatusPanelModel {
        title: "Hook status",
        shell_label: "Shell Hooks",
        shell_lines: vec![
            "Registered: 2; enabled: 2; disabled: 0.".to_string(),
            "Sources: builtin=2; user=0; project=0.".to_string(),
        ],
        shell_entries: vec![
            shell_entry("high-memory-process", "builtin", false),
            shell_entry("memory-pressure", "builtin", false),
        ],
        agent_label: "Agent Hooks",
        agent: AgentHooksView::Groups(vec![
            HookEventGroup {
                event: "PreToolUse".to_string(),
                hooks: vec![
                    shell_entry("skill-ledger", "ext: agent-sec-core", false),
                    shell_entry("pii-checker", "ext: agent-sec-core", true),
                ],
            },
            HookEventGroup {
                event: "Stop".to_string(),
                hooks: vec![shell_entry(
                    "observability-hook",
                    "ext: agent-sec-core",
                    false,
                )],
            },
        ]),
        omitted_template: "… {count} more hook(s) not shown".to_string(),
        footer: "3 hook(s) registered.".to_string(),
    }
}

#[test]
fn hook_status_panel_layers_sections_events_and_entries() {
    let renderer = RatatuiInlineRenderer::with_width(80);
    let mut output = Vec::new();
    renderer
        .write_hook_status_panel(&mut output, sample_model())
        .unwrap();

    let text = strip_ansi_escape(&String::from_utf8(output).unwrap());
    let lines = text.lines().collect::<Vec<_>>();
    // Section headers at column zero, like /help group headers.
    assert!(
        lines.iter().any(|line| line.starts_with("│ Shell Hooks")),
        "{text}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("│ Agent Hooks")),
        "{text}"
    );
    // Shell stats indented under their section.
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("│   Registered: 2")),
        "{text}"
    );
    // Event group headers indented by two, entries by four.
    assert!(
        lines.iter().any(|line| line.starts_with("│   PreToolUse")),
        "{text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("│     • high-memory-process (builtin)")),
        "{text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("│     • memory-pressure (builtin)")),
        "{text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("│     • skill-ledger (ext: agent-sec-core)")),
        "{text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("│     ○ pii-checker (ext: agent-sec-core) [disabled]")),
        "{text}"
    );
    // No legacy per-line event suffix.
    assert!(!text.contains("[PreToolUse]"), "{text}");
    // Footer inside the panel body.
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("│ 3 hook(s) registered.")),
        "{text}"
    );
}

#[test]
fn hook_status_panel_strips_control_sequences_from_registry_text() {
    // Registry-provided names/extensions and error messages are untrusted:
    // embedded escape sequences must never reach the terminal (they could
    // recolor or clear the screen), on either backend.
    let mut model = sample_model();
    model.agent = AgentHooksView::Groups(vec![HookEventGroup {
        event: "Pre\u{1b}[31mToolUse".to_string(),
        hooks: vec![HookEntryView {
            name: "evil\u{1b}[31mred".to_string(),
            detail: "ext: wipe\u{1b}[2Jext".to_string(),
            disabled: false,
        }],
    }]);

    let mut styled = RatatuiInlineRenderer::with_width(100);
    styled.styled = true;
    let mut output = Vec::new();
    styled.write_hook_status_panel(&mut output, model).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains("\u{1b}[31m"), "{text:?}");
    assert!(!text.contains("\u{1b}[2J"), "{text:?}");
    assert!(text.contains("evilred"), "{text}");
    assert!(
        text.contains("wipeJext") || text.contains("wipeext"),
        "{text}"
    );

    let mut model = sample_model();
    model.agent = AgentHooksView::Message("boom\u{1b}[2J\rcleared".to_string());
    let plain = RatatuiInlineRenderer::plain_with_width(100);
    let mut output = Vec::new();
    plain.write_hook_status_panel(&mut output, model).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(!text.contains('\r'), "{text:?}");
}

#[test]
fn hook_status_panel_wraps_long_entries_with_hanging_indent_when_narrow() {
    let long_entry = HookEntryView {
        name: "very-long-hook-name-that-wraps".to_string(),
        detail: "ext: very-long-extension-name".to_string(),
        disabled: false,
    };
    let model = HookStatusPanelModel {
        title: "Hook status",
        shell_label: "Shell Hooks",
        shell_lines: vec![
            "Registered: 2; enabled: 2; disabled: 0. Sources: builtin=2; user=0; project=0."
                .to_string(),
        ],
        shell_entries: vec![],
        agent_label: "Agent Hooks",
        agent: AgentHooksView::Groups(vec![HookEventGroup {
            event: "PreToolUse".to_string(),
            hooks: vec![long_entry],
        }]),
        omitted_template: "… {count} more hook(s) not shown".to_string(),
        footer: "1 hook(s) registered.".to_string(),
    };

    let renderer = RatatuiInlineRenderer::with_width(40);
    let mut output = Vec::new();
    renderer
        .write_hook_status_panel(&mut output, model)
        .unwrap();

    let text = strip_ansi_escape(&String::from_utf8(output).unwrap());
    let lines = text.lines().collect::<Vec<_>>();
    // The entry wraps; its continuation keeps the hanging indent instead of
    // falling back to column zero.
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("│     • very-long-hook-name")),
        "{text}"
    );
    let entry_index = lines
        .iter()
        .position(|line| line.starts_with("│     • very-long-hook-name"))
        .unwrap();
    assert!(
        lines[entry_index + 1].starts_with("│       "),
        "continuation lost its hanging indent: {text}"
    );
    // Shell stats continuations stay indented under their section too.
    let stats_index = lines
        .iter()
        .position(|line| line.starts_with("│   Registered: 2"))
        .unwrap();
    assert!(
        lines[stats_index + 1].starts_with("│   "),
        "stats continuation returned to column zero: {text}"
    );

    // Plain backend wraps to the same width contract instead of overflowing.
    let plain = RatatuiInlineRenderer::plain_with_width(40);
    let model = HookStatusPanelModel {
        title: "Hook status",
        shell_label: "Shell Hooks",
        shell_lines: vec!["Registered: 2; enabled: 2; disabled: 0.".to_string()],
        shell_entries: vec![],
        agent_label: "Agent Hooks",
        agent: AgentHooksView::Groups(vec![HookEventGroup {
            event: "PreToolUse".to_string(),
            hooks: vec![HookEntryView {
                name: "very-long-hook-name-that-wraps".to_string(),
                detail: "ext: very-long-extension-name".to_string(),
                disabled: false,
            }],
        }]),
        omitted_template: "… {count} more hook(s) not shown".to_string(),
        footer: "1 hook(s) registered.".to_string(),
    };
    let mut output = Vec::new();
    plain.write_hook_status_panel(&mut output, model).unwrap();
    let text = String::from_utf8(output).unwrap();
    // The plain renderer is constructed with width 40; every emitted line
    // must stay within that display-width contract (not merely a loose
    // char-count bound).
    assert!(
        text.lines().all(|line| display_width(line) <= 40),
        "plain line overflowed the width contract: {text}"
    );
    assert!(text.contains("very-long-hook-name-that-wraps"), "{text}");
}

#[test]
fn hook_status_panel_large_registry_truncates_with_marker_and_keeps_footer() {
    // The rich block renderer caps panels at 200 buffer rows; a large
    // registry must degrade to an explicit omission marker instead of
    // Paragraph silently dropping the tail (including the footer).
    let groups: Vec<HookEventGroup> = (0..12)
        .map(|group_index| HookEventGroup {
            event: format!("Event-{group_index}"),
            hooks: (0..15)
                .map(|hook_index| HookEntryView {
                    name: format!("observability-hook-{group_index}-{hook_index}"),
                    detail: "ext: agent-sec-core".to_string(),
                    disabled: false,
                })
                .collect(),
        })
        .collect();
    let model = HookStatusPanelModel {
        title: "Hook status",
        shell_label: "Shell Hooks",
        shell_lines: vec!["Registered: 2; enabled: 2; disabled: 0.".to_string()],
        shell_entries: vec![],
        agent_label: "Agent Hooks",
        agent: AgentHooksView::Groups(groups.clone()),
        omitted_template: "… {count} more hook(s) not shown (full list in plain output)"
            .to_string(),
        footer: "180 hook(s) registered.".to_string(),
    };

    let renderer = RatatuiInlineRenderer::with_width(40);
    let mut output = Vec::new();
    renderer
        .write_hook_status_panel(&mut output, model)
        .unwrap();

    let text = strip_ansi_escape(&String::from_utf8(output).unwrap());
    // Footer survives, an omission marker is present, and nothing overflows
    // the renderer's 200-row cap.
    assert!(text.contains("180 hook(s) registered."), "{text}");
    assert!(text.contains("more hook(s) not shown"), "{text}");
    assert!(text.lines().count() <= 200, "{}", text.lines().count());
    let marker_line = text.lines().find(|line| line.contains('…')).unwrap();
    let omitted: usize = marker_line
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap();
    assert!(omitted > 0 && omitted < 180, "{marker_line}");

    // The plain backend never truncates: all hooks and the footer remain.
    let plain = RatatuiInlineRenderer::plain_with_width(120);
    let model = HookStatusPanelModel {
        title: "Hook status",
        shell_label: "Shell Hooks",
        shell_lines: vec!["Registered: 2; enabled: 2; disabled: 0.".to_string()],
        shell_entries: vec![],
        agent_label: "Agent Hooks",
        agent: AgentHooksView::Groups(groups),
        omitted_template: "… {count} more hook(s) not shown".to_string(),
        footer: "180 hook(s) registered.".to_string(),
    };
    let mut output = Vec::new();
    plain.write_hook_status_panel(&mut output, model).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("hook-11-14"), "{text}");
    assert!(text.contains("180 hook(s) registered."), "{text}");
    assert!(!text.contains("more hook(s) not shown"), "{text}");
}

#[test]
fn hook_status_panel_omits_an_entry_that_cannot_fit_atomically() {
    let model = HookStatusPanelModel {
        title: "Hook status",
        shell_label: "Shell Hooks",
        shell_lines: vec!["Registered: 1; enabled: 1; disabled: 0.".to_string()],
        shell_entries: vec![],
        agent_label: "Agent Hooks",
        agent: AgentHooksView::Groups(vec![HookEventGroup {
            event: "PreToolUse".to_string(),
            hooks: vec![HookEntryView {
                name: "x".repeat(8_000),
                detail: "ext: agent-sec-core".to_string(),
                disabled: false,
            }],
        }]),
        omitted_template: "… {count} more hook(s) not shown (full list in plain output)"
            .to_string(),
        footer: "1 hook(s) registered.".to_string(),
    };

    let renderer = RatatuiInlineRenderer::with_width(40);
    let mut output = Vec::new();
    renderer
        .write_hook_status_panel(&mut output, model)
        .unwrap();

    let text = strip_ansi_escape(&String::from_utf8(output).unwrap());
    assert!(text.contains("1 more hook(s) not shown"), "{text}");
    assert!(text.contains("1 hook(s) registered."), "{text}");
    assert!(!text.contains(&"x".repeat(20)), "{text}");
    assert!(text.lines().count() <= 200, "{}", text.lines().count());
}

#[test]
fn hook_status_panel_message_view_and_plain_backend_degrade() {
    let mut model = sample_model();
    model.agent = AgentHooksView::Message("(none)".to_string());

    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let mut output = Vec::new();
    renderer
        .write_hook_status_panel(&mut output, model)
        .unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("Hook status:"), "{text}");
    assert!(text.contains("── Shell Hooks ──"), "{text}");
    assert!(text.contains("── Agent Hooks ──"), "{text}");
    assert!(text.contains("(none)"), "{text}");
    assert!(text.contains("3 hook(s) registered."), "{text}");
    assert!(!text.contains('│'), "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
}
