use super::*;
use ratatui::{backend::TestBackend, Terminal};

fn view() -> WizardView {
    WizardView {
        title: "Cosh setup and getting started".to_string(),
        rows: vec![
            WizardRow::Choice {
                label: "Language".to_string(),
                value: "English".to_string(),
            },
            WizardRow::Choice {
                label: "AI service".to_string(),
                value: String::new(),
            },
            WizardRow::Blank,
            WizardRow::Choice {
                label: "Getting started".to_string(),
                value: "Commands, questions, and exit".to_string(),
            },
        ],
        cursor: 0,
        hint: "↑↓ select  Enter open  Esc quit".to_string(),
        message: None,
        markdown: None,
        scroll: 0,
        language: Language::EnUs,
        message_fresh: false,
        follow_cursor: true,
    }
}

fn render_lines(width: u16, height: u16, view: &WizardView) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            super::render(frame, view);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn standard_terminal_uses_one_centered_column_within_the_content_width() {
    let lines = render_lines(80, 24, &view());
    assert!(lines[1].contains("Cosh setup and getting started"));
    assert!(lines[3].contains("> Language"));
    assert!(lines[3].contains("English"));
    for line in &lines {
        assert!(display_width(line) <= 78, "{line}");
    }
    let left = lines[3].find('>').unwrap();
    assert!(left >= 1, "column must keep a side margin: {left}");
}

#[test]
fn only_the_focused_row_is_reversed() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| {
            super::render(frame, &view());
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let focused = buffer[(2, 3)].style();
    let other = buffer[(2, 4)].style();
    assert!(focused.add_modifier.contains(Modifier::REVERSED));
    assert!(!other.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn narrow_terminal_keeps_a_single_column_and_truncates_long_values() {
    let mut long = view();
    long.rows[0] = WizardRow::Choice {
        label: "A very long setting label that cannot fit".to_string(),
        value: "a-very-long-value".to_string(),
    };
    let lines = render_lines(40, 16, &long);
    assert!(lines[1].contains("Cosh setup"));
    assert!(lines[3].contains('>'));
    for line in &lines {
        assert!(display_width(line) <= 40, "{line}");
    }
    assert!(lines[3].contains('…'), "{}", lines[3]);
}

#[test]
fn choice_values_preserve_labels_within_the_content_width() {
    for label in ["AI service", "AI 服务", &"L".repeat(36)] {
        for value in [String::new(), "on".to_string(), "long value ".repeat(10)] {
            for width in [38, 76] {
                for selected in [false, true] {
                    let line = choice_line(label, &value, width, selected);
                    let text = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    assert!(text.contains(label), "{width}: {text}");
                    assert!(line.width() <= width, "{width}: {text}");
                }
            }
        }
    }
}

#[test]
fn undersized_terminal_shows_the_compact_notice_instead_of_the_form() {
    let lines = render_lines(30, 10, &view());
    let text = lines.join("\n");
    assert!(text.contains("Terminal too small"));
    assert!(text.contains("cosh guide --plain"));
    assert!(!text.contains("> Language"));
}

#[test]
fn markdown_page_renders_shared_content_and_a_scroll_position() {
    let mut page = view();
    page.rows.clear();
    page.markdown = Some(
        crate::config::guide::wizard::content::start_markdown(crate::I18n::new(Language::EnUs))
            .to_string(),
    );
    page.scroll = 2;
    let lines = render_lines(40, 16, &page);
    let text = lines.join("\n");
    assert!(text.contains("cosh --ask"), "{text}");
    let has_indicator = lines.iter().any(|line| {
        let mut parts = line.trim().split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), None) => {
                !a.is_empty()
                    && !b.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && b.chars().all(|c| c.is_ascii_digit())
            }
            _ => false,
        }
    });
    assert!(has_indicator, "scroll position missing: {text}");
}

#[test]
fn markdown_pages_fit_the_render_buffer_at_supported_sizes() {
    for language in [Language::EnUs, Language::ZhCn] {
        let i18n = crate::I18n::new(language);
        let pages = [
            (
                crate::config::guide::wizard::content::start_markdown(i18n),
                "cosh --ask",
                match language {
                    Language::EnUs => "ends the login.",
                    _ => "会结束登录会话。",
                },
            ),
            (
                i18n.t(crate::i18n::MessageId::GuideWizardDataPrivacy),
                match language {
                    Language::EnUs => "Bash history is not imported by default",
                    _ => "默认不导入 Bash 历史",
                },
                match language {
                    Language::EnUs => "provider records.",
                    _ => "服务方记录。",
                },
            ),
        ];
        for (markdown, head_anchor, tail) in pages {
            let mut page = view();
            page.rows.clear();
            page.language = language;
            page.markdown = Some(markdown.to_string());
            for (width, height) in [(80, 24), (80, 30), (100, 30), (40, 24), (40, 16), (120, 40)] {
                page.scroll = 0;
                let head = render_lines(width, height, &page).join("\n");
                let compact_head: String =
                    head.chars().filter(|c| !c.is_whitespace()).collect();
                assert!(
                    compact_head
                        .contains(&head_anchor.chars().filter(|c| !c.is_whitespace()).collect::<String>()),
                    "{language:?} {width}x{height} head: {head}"
                );
                page.scroll = 999;
                let bottom = render_lines(width, height, &page).join("\n");
                let compact: String = bottom.chars().filter(|c| !c.is_whitespace()).collect();
                assert!(
                    compact.contains(
                        &tail.chars().filter(|c| !c.is_whitespace()).collect::<String>()
                    ),
                    "{language:?} {width}x{height} tail unreachable: {bottom}"
                );
            }
        }
    }
}

#[test]
fn markdown_render_clamps_oversized_scroll() {
    let mut page = view();
    page.rows.clear();
    page.markdown = Some(
        (0..120)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    page.scroll = 999;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut returned = usize::MAX;
    terminal
        .draw(|frame| {
            returned = super::render(frame, &page);
        })
        .unwrap();
    assert!(returned < 999, "scroll must be clamped: {returned}");
    assert!(
        returned > 0,
        "long content should allow scrolling: {returned}"
    );
}

#[test]
fn quick_guide_separates_sections_and_isolates_the_copyable_command() {
    fn compact(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }
    for (language, heading, command) in [
        (
            Language::ZhCn,
            "在 Bash 中按需提问",
            "cosh --ask '磁盘不足时先检查什么？'",
        ),
        (
            Language::EnUs,
            "Ask from Bash when you need it",
            "cosh --ask 'Why is my disk full?'",
        ),
    ] {
        let mut page = view();
        page.rows.clear();
        page.language = language;
        page.markdown = Some(
            crate::config::guide::wizard::content::start_markdown(crate::I18n::new(language))
                .to_string(),
        );
        let lines = render_lines(100, 30, &page);
        let head = lines
            .iter()
            .position(|line| compact(line) == compact(heading))
            .unwrap_or_else(|| panic!("{language:?} heading missing: {lines:?}"));
        assert!(
            lines[head - 1].trim().is_empty(),
            "{language:?} section not separated: {lines:?}"
        );
        assert!(
            lines[head + 1].contains('─') && !lines[head + 2].trim().is_empty(),
            "{language:?} heading detached from its body: {lines:?}"
        );
        let cmd = lines
            .iter()
            .position(|line| compact(line) == compact(command))
            .unwrap_or_else(|| panic!("{language:?} command not on its own row: {lines:?}"));
        assert!(
            lines[cmd - 1].trim().is_empty() && lines[cmd + 1].trim().is_empty(),
            "{language:?} command not set apart: {lines:?}"
        );
    }
}

#[test]
fn markdown_scroll_reaches_the_last_line() {
    let mut page = view();
    page.rows.clear();
    page.markdown = Some(
        (0..30)
            .map(|i| format!("para {i}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    page.scroll = 999;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| {
            super::render(frame, &page);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let text = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("para 29"), "last line unreachable:\n{text}");
}

#[test]
fn markdown_word_wrap_does_not_drop_trailing_words() {
    let mut page = view();
    page.rows.clear();
    let w1 = "a".repeat(40);
    let w2 = "b".repeat(40);
    let w3 = "c".repeat(40);
    page.markdown = Some(format!("{w1} {w2} {w3}"));
    let lines = render_lines(80, 24, &page);
    let text = lines.join("\n");
    assert!(text.contains(&w1), "first word missing:\n{text}");
    assert!(
        text.contains(&w3),
        "word-wrap dropped trailing word:\n{text}"
    );
}

#[test]
fn form_scrolls_to_keep_the_focused_row_visible() {
    let mut page = view();
    page.rows = vec![
        WizardRow::Text("Bash history import: On".to_string()),
        WizardRow::Text("Saved: On".to_string()),
        WizardRow::Text("Source: environment".to_string()),
        WizardRow::Blank,
        WizardRow::Choice {
            label: "Save preference (off)".to_string(),
            value: String::new(),
        },
        WizardRow::Text("Effective state is still controlled by the environment.".to_string()),
        WizardRow::Text("Stopping keeps existing data.".to_string()),
        WizardRow::Blank,
        WizardRow::Choice {
            label: "Clear recommendation data".to_string(),
            value: String::new(),
        },
        WizardRow::Choice {
            label: "Reset damaged data".to_string(),
            value: String::new(),
        },
        WizardRow::Blank,
        WizardRow::Choice {
            label: "Privacy details".to_string(),
            value: String::new(),
        },
    ];
    page.cursor = 11;
    page.message = Some("Saved: On".to_string());
    let lines = render_lines(40, 16, &page);
    let text = lines.join("\n");
    assert!(
        text.contains("Privacy details"),
        "focused row clipped:\n{text}"
    );
    assert!(
        !text.contains("Bash history import"),
        "form did not scroll:\n{text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("> Privacy details")),
        "focused row not highlighted:\n{text}"
    );
}

#[test]
fn zh_privacy_last_sentence_is_reachable_at_40x16() {
    let mut page = view();
    page.rows.clear();
    page.language = Language::ZhCn;
    page.markdown = Some(
        crate::I18n::new(Language::ZhCn)
            .t(crate::i18n::MessageId::GuideWizardDataPrivacy)
            .to_string(),
    );
    page.scroll = 999;
    let lines = render_lines(40, 16, &page);
    let text = lines.join("\n");
    // Wide CJK glyphs occupy two buffer cells; per-cell extraction leaves a
    // space between characters, so compare on whitespace-compacted text.
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        compact.contains("服务方记录。"),
        "zh privacy tail unreachable at 40x16:\n{text}"
    );
}

fn confirmation_rows() -> Vec<WizardRow> {
    vec![
        WizardRow::Text(
            "Turning this off clears local recommendation activity, profiles, and caches. \
             Existing Cosh sessions are not updated. Source Bash history, credentials, and \
             server-side records are kept."
                .to_string(),
        ),
        WizardRow::Blank,
        WizardRow::Choice {
            label: "Confirm".to_string(),
            value: String::new(),
        },
        WizardRow::Choice {
            label: "Cancel".to_string(),
            value: String::new(),
        },
    ]
}

#[test]
fn destructive_disclosure_keeps_its_last_sentence_reachable_at_40x16() {
    let mut page = view();
    page.rows = confirmation_rows();
    page.cursor = 3;
    page.scroll = 999;
    let lines = render_lines(40, 16, &page);
    let text = lines.join("\n");
    assert!(
        text.contains("server-side records are kept."),
        "disclosure tail unreachable:\n{text}"
    );
}

#[test]
fn long_result_moves_into_the_body_and_scrolls_to_its_tail() {
    let mut page = view();
    page.message = Some(
        "Saved the AI service switch. The delete result is unconfirmed; re-read the service \
         list before retrying. The zero-config access preference was partly completed, so the \
         session record may still be pending. Nothing was rolled back: the switch you already \
         made stays in place. New sessions use the saved value."
            .to_string(),
    );
    page.message_fresh = true;
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
    let mut returned = usize::MAX;
    terminal
        .draw(|frame| {
            returned = super::render(frame, &page);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let text = (0..16)
        .map(|y| {
            (0..40)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(returned > 0, "long result must be scrollable: {returned}");
    let compact: String = text.chars().filter(|character| !character.is_whitespace()).collect();
    assert!(
        compact.contains("Newsessionsusethesavedvalue."),
        "result tail unreachable:\n{text}"
    );
}

#[test]
fn short_result_stays_on_one_inline_line() {
    let mut page = view();
    page.message = Some("Saved: On".to_string());
    page.message_fresh = true;
    let lines = render_lines(80, 24, &page);
    assert!(lines.iter().any(|line| line.contains("Saved: On")));
    assert!(lines[3].contains("> Language"));
}

#[test]
fn zh_result_tail_is_reachable_at_40x16() {
    let mut page = view();
    page.language = Language::ZhCn;
    page.message = Some(
        "已保存关闭偏好；本地推荐数据的清理结果尚未确认。已有 Cosh 会话未同步，新会话按保存值运行。"
            .to_string(),
    );
    page.message_fresh = true;
    let lines = render_lines(40, 16, &page);
    let compact: String = lines
        .join("\n")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    assert!(
        compact.contains("新会话按保存值运行。"),
        "zh result tail unreachable:\n{compact}"
    );
}
