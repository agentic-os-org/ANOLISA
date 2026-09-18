#![cfg(target_os = "linux")]

use super::support::terminal_screen::TerminalSession;

const COLS: u16 = 24;
const ROWS: u16 = 24;

fn expected_screen(prompt: &str, command: &str, cursor_prefix: Option<&str>) -> vt100::Parser {
    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    parser.process(prompt.as_bytes());
    parser.process(command.as_bytes());
    if let Some(prefix) = cursor_prefix {
        let mut cursor = vt100::Parser::new(ROWS, COLS, 0);
        cursor.process(prompt.as_bytes());
        cursor.process(prefix.as_bytes());
        let (row, col) = cursor.screen().cursor_position();
        parser.process(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    }
    parser
}

fn same_cells(actual: &vt100::Screen, expected: &vt100::Screen) -> bool {
    actual.cursor_position() == expected.cursor_position()
        && (0..ROWS).all(|row| {
            (0..COLS).all(|col| {
                let actual = actual.cell(row, col).expect("actual screen cell");
                let expected = expected.cell(row, col).expect("expected screen cell");
                let actual_text = if actual.has_contents() {
                    actual.contents()
                } else {
                    " "
                };
                let expected_text = if expected.has_contents() {
                    expected.contents()
                } else {
                    " "
                };
                actual_text == expected_text
                    && actual.is_wide() == expected.is_wide()
                    && actual.is_wide_continuation() == expected.is_wide_continuation()
            })
        })
}

fn wait_command(
    session: &mut TerminalSession,
    label: &str,
    command: &str,
    cursor_prefix: Option<&str>,
) {
    // A literal reference transcript supplies desired cells independently of
    // Readline/ZLE's redraw bytes. The parser retains wide and combining cells.
    let expected = expected_screen(&session.published_prompt(), command, cursor_prefix);
    session.wait_screen(label, |screen| same_cells(screen, expected.screen()));
}

fn assert_unicode_line_editing(shell: &str, integration: &str, combining_chars: bool) {
    let context = format!("{shell}/{integration}/combining_chars={combining_chars}");
    let mut session = TerminalSession::spawn_for_shell(shell, integration, COLS);
    let receipt = session.home().join("unicode-result");
    let startup = if shell == "zsh" && combining_chars {
        "setopt COMBINING_CHARS; "
    } else {
        ""
    };
    session.send(
        &[
            startup.as_bytes(),
            b"u() { printf '%s' \"$1\" > \"$HOME/unicode-result\"; }; printf '%s%s\\n' '__UNICODE_' 'READY__'\n",
        ]
        .concat(),
    );
    session.wait_screen(&format!("{context}: fixture ready"), |screen| {
        screen.contents().contains("__UNICODE_READY__")
    });
    // A shell command clears setup output before the next prompt is emitted.
    session.send(b"printf '\\033[H\\033[2J'\n");
    wait_command(
        &mut session,
        &format!("{context}: cleared prompt"),
        "",
        None,
    );

    session.send("u '0123456789界QR".as_bytes());
    wait_command(
        &mut session,
        &format!("{context}: wide input wraps"),
        "u '0123456789界QR",
        None,
    );
    assert!(
        (0..ROWS).any(|row| (0..COLS - 1).any(|col| {
            let cell = session.screen().cell(row, col).expect("wide cell");
            cell.contents() == "界"
                && cell.is_wide()
                && session
                    .screen()
                    .cell(row, col + 1)
                    .expect("wide continuation")
                    .is_wide_continuation()
        })),
        "{context}: CJK must occupy two terminal cells"
    );
    session.send(b"\x1b[D\x1b[D");
    wait_command(
        &mut session,
        &format!("{context}: move before suffix"),
        "u '0123456789界QR",
        Some("u '0123456789界"),
    );
    session.send(b"\x7f");
    wait_command(
        &mut session,
        &format!("{context}: erase wide character"),
        "u '0123456789QR",
        Some("u '0123456789"),
    );
    session.send("中".as_bytes());
    wait_command(
        &mut session,
        &format!("{context}: insert replacement wide character"),
        "u '0123456789中QR",
        Some("u '0123456789中"),
    );
    session.send(b"\x05\x7f\x7f");
    wait_command(
        &mut session,
        &format!("{context}: remove ASCII suffix"),
        "u '0123456789中",
        None,
    );
    session.send("e\u{301}'".as_bytes());
    // Zsh's default COMBINING_CHARS=off renders a zero-width character
    // as markup; its input buffer still contains the original UTF-8 bytes.
    let command = if shell == "zsh" && !combining_chars {
        "u '0123456789中e<0301>'"
    } else {
        "u '0123456789中e\u{301}'"
    };
    wait_command(
        &mut session,
        &format!("{context}: combining sequence follows shell display policy"),
        command,
        None,
    );
    if shell == "bash" || combining_chars {
        let screen = session.screen();
        assert!(
            (0..ROWS).any(|row| (0..COLS).any(|col| {
                let cell = screen.cell(row, col).expect("combining cell");
                cell.contents() == "e\u{301}" && !cell.is_wide()
            })),
            "{context}: {}",
            screen.contents()
        );
    }

    // Home/End traverse the complete Unicode input without requiring
    // Bash and Zsh to share a combining-mark deletion policy.
    session.send(b"\x01");
    wait_command(
        &mut session,
        &format!("{context}: home before wrapped input"),
        command,
        Some(""),
    );
    session.send(b"\x05");
    wait_command(
        &mut session,
        &format!("{context}: end after combining sequence"),
        command,
        None,
    );
    session.send(b"\n");
    let expected_bytes = "0123456789中e\u{301}".as_bytes();
    session.wait_screen(&format!("{context}: execution receipt"), |_| {
        std::fs::read(&receipt).is_ok_and(|bytes| bytes == expected_bytes)
    });
    assert_eq!(
        std::fs::read(&receipt).expect("execution bytes"),
        expected_bytes,
        "{context}"
    );
    // Dirty Bash drafts use the existing privacy guard's leading blank at
    // accept-line. Preserve that exact display policy without changing the
    // pre-submit cells or the original argument's execution bytes.
    let private_blank = if shell == "bash" && integration == "enhanced" {
        " "
    } else {
        ""
    };
    let submitted = format!("{private_blank}{command}\r\n{}", session.published_prompt());
    wait_command(
        &mut session,
        &format!("{context}: submitted screen and next prompt"),
        &submitted,
        None,
    );
    session.finish();
}

#[test]
fn raw_cli_bash_native_unicode_cells_cursor_and_execution() {
    assert_unicode_line_editing("bash", "native", false);
}

#[test]
fn raw_cli_bash_enhanced_unicode_cells_cursor_and_execution() {
    assert_unicode_line_editing("bash", "enhanced", false);
}

#[test]
fn raw_cli_zsh_native_unicode_cells_cursor_and_execution() {
    assert_unicode_line_editing("zsh", "native", false);
}

#[test]
fn raw_cli_zsh_enhanced_unicode_cells_cursor_and_execution() {
    assert_unicode_line_editing("zsh", "enhanced", false);
}

#[test]
fn raw_cli_zsh_native_combining_cells_cursor_and_execution() {
    assert_unicode_line_editing("zsh", "native", true);
}

#[test]
fn raw_cli_zsh_enhanced_combining_cells_cursor_and_execution() {
    assert_unicode_line_editing("zsh", "enhanced", true);
}
