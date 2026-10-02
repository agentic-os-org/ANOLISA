use crate::ui::wrap::{display_width, wrap_plain_line};

#[test]
fn wrap_plain_line_fills_remaining_width_with_cjk() {
    let width = 10;
    let lines = wrap_plain_line("abc 中文中文中文中", width);

    assert_eq!(lines, ["abc 中文中", "文中文中"]);
    assert_eq!(display_width(&lines[0]), width);
    assert!(lines.iter().all(|line| display_width(line) <= width));

    assert_eq!(
        wrap_plain_line("abc workspace", 10),
        ["abc", "workspace"],
        "an ASCII word should move intact instead of filling the previous line"
    );
}

#[test]
fn wrap_plain_line_preserves_non_cjk_text_units() {
    assert_eq!(wrap_plain_line("abc café", 7), ["abc", "café"]);
    assert_eq!(wrap_plain_line("abc 👩‍💻", 6), ["abc 👩‍💻"]);
}

#[test]
fn wrap_plain_line_fills_governance_hook_message_rows() {
    let width = 40;
    let lines = wrap_plain_line(
        "Message: [pii-checker] 检测到 4 项高风险敏感信息；当前策略仅提醒，本次不会阻断。",
        width,
    );

    assert_eq!(lines[0], "Message: [pii-checker] 检测到 4 项高风险");
    assert_eq!(display_width(&lines[0]), width);
    assert!(lines.iter().all(|line| display_width(line) <= width));
}

#[test]
fn wrap_plain_line_keeps_ascii_identifier_among_cjk_punctuation() {
    let width = 20;
    let lines = wrap_plain_line("检查request_id=abc123，状态正常", width);

    assert_eq!(lines, ["检查", "request_id=abc123，", "状态正常"]);
    assert!(lines.iter().all(|line| display_width(line) <= width));
}

#[test]
fn wrap_plain_line_keeps_line_punctuation_with_text() {
    let width = 8;

    for punctuation in ['。', '”', '’', '」', '』', '〉'] {
        let input = format!("abc 中文{punctuation}");
        let lines = wrap_plain_line(&input, width);

        assert_eq!(lines, ["abc 中", &format!("文{punctuation}")]);
        assert!(lines.iter().all(|line| display_width(line) <= width));
    }

    for punctuation in ['（', '“', '‘', '「', '『', '〈'] {
        let input = format!("abc 中{punctuation}文");
        let lines = wrap_plain_line(&input, width);

        assert_eq!(lines, ["abc 中", &format!("{punctuation}文")]);
        assert!(lines.iter().all(|line| display_width(line) <= width));
    }

    assert_eq!(
        wrap_plain_line("abc 中文)request_id", 15),
        ["abc 中", "文)request_id"],
        "a closer-prefixed ASCII token should stay with the preceding CJK unit"
    );
}

#[test]
fn wrap_plain_line_uses_remaining_width_with_narrow_prefixes() {
    let width = 12;
    let list = wrap_plain_line("- abc 中文中文中文", width);
    let indented = wrap_plain_line("    abc 中文中文中文", width);

    assert_eq!(list, ["- abc 中文中", "  文中文"]);
    assert_eq!(indented, ["    abc 中文", "    中文中文"]);
    assert!(list
        .iter()
        .chain(&indented)
        .all(|line| display_width(line) <= width));
}
