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
    assert!(text.contains("cosh --ask 'one question'"), "{text}");
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
