use crate::config::Language;
use crate::ui::trial_layout::{render_trial_waiting, TrialLayout, TrialLayoutKind};
use crate::ui::wrap::strip_ansi_escape;
use unicode_width::UnicodeWidthStr;

const CONSOLE_URL: &str = "https://console.aliyun.com/alinux/welcome?source=cosh";

fn plain(layout: &TrialLayout) -> Vec<String> {
    layout
        .lines
        .iter()
        .map(|line| strip_ansi_escape(line))
        .collect()
}

fn assert_fits(layout: &TrialLayout, cols: u16, rows: u16, reserved: u16) {
    assert_eq!(layout.height as usize, layout.lines.len());
    assert!(layout.height <= rows.saturating_sub(reserved));
    for line in plain(layout) {
        assert!(line.width() <= usize::from(cols), "{line:?}");
    }
}

#[test]
fn tall_terminal_has_normal_left_aligned_card() {
    let layout = render_trial_waiting(80, 40, 3, Language::ZhCn);
    assert_eq!(layout.layout_kind, TrialLayoutKind::Normal);
    assert_eq!(layout.height, 34);
    assert_fits(&layout, 80, 40, 3);
    let lines = plain(&layout);
    assert_eq!(lines[0].width(), 57);
    assert!(lines
        .iter()
        .any(|line| line.contains("AI Shell · 免配置接入")));
    assert!(lines.iter().any(|line| line.contains("操作系统控制台")));
    assert!(lines.iter().any(|line| line.contains("当前 ECS 所属账号")));
    assert!(lines.iter().any(|line| line.contains("/auth")));
    let qr = layout
        .lines
        .iter()
        .position(|line| line.contains("\x1b[38;2;"))
        .unwrap();
    let link = lines
        .iter()
        .find(|line| line.contains(CONSOLE_URL))
        .unwrap();
    assert_eq!(link.find('h'), Some(4));
    assert!(layout.lines[qr].starts_with("│ "));
}

#[test]
fn short_terminal_uses_compact_only_when_it_really_fits() {
    for cols in [80, 60] {
        let compact = render_trial_waiting(cols, 28, 1, Language::ZhCn);
        assert_eq!(compact.layout_kind, TrialLayoutKind::Compact);
        assert_eq!(compact.height, 26);
        assert_fits(&compact, cols, 28, 1);
        let link = render_trial_waiting(cols, 24, 2, Language::ZhCn);
        assert_eq!(link.layout_kind, TrialLayoutKind::Link);
        assert_fits(&link, cols, 24, 2);
    }
}

#[test]
fn width_height_and_language_all_participate_in_layout() {
    for (cols, rows) in [(40, 24), (80, 12), (36, 40)] {
        for language in [Language::ZhCn, Language::EnUs] {
            let layout = render_trial_waiting(cols, rows, 2, language);
            assert_eq!(layout.layout_kind, TrialLayoutKind::Link);
            assert_fits(&layout, cols, rows, 2);
            let text = plain(&layout).join("");
            assert!(text.contains(CONSOLE_URL));
            assert!(text.contains("Esc"));
            assert!(text.contains("/auth"));
            assert!(text.contains("n "));
            assert!(!layout.lines.iter().any(|line| line.contains("\x1b[38;2;")));
        }
    }
}

#[test]
fn tiny_windows_keep_a_cancel_hint_without_overflow() {
    for (cols, rows, reserved) in [
        (8, 2, 1),
        (3, 1, 0),
        (2, 1, 0),
        (1, 1, 0),
        (0, 2, 0),
        (80, 2, 2),
    ] {
        let layout = render_trial_waiting(cols, rows, reserved, Language::ZhCn);
        assert_fits(&layout, cols, rows, reserved);
        assert_eq!(layout.layout_kind, TrialLayoutKind::Minimal);
        if cols > 0 && rows > reserved {
            assert!(!layout.lines.is_empty());
        }
    }
}

#[test]
fn qr_keeps_four_module_quiet_zone_and_fixed_white_background() {
    let layout = render_trial_waiting(80, 28, 1, Language::ZhCn);
    let qr: Vec<_> = layout
        .lines
        .iter()
        .filter(|line| line.contains("\x1b[38;2;"))
        .collect();
    assert_eq!(qr.len(), 21);
    let white = " ".repeat(41);
    for (index, line) in qr.iter().enumerate() {
        assert!(line.starts_with("\x1b[38;2;0;0;0;48;2;255;255;255m"));
        assert!(line.ends_with("    \x1b[0m"));
        let cells = strip_ansi_escape(line);
        assert_eq!(cells.width(), 41);
        assert!(cells.starts_with("    "));
        assert!(cells.ends_with("    "));
        if !(2..19).contains(&index) {
            assert_eq!(cells, white);
        }
    }
}
