#[test]
fn plain_text_is_unchanged() {
    let text = "make: Entering directory '/work'\ncc -O2 -c main.c\nmake: Leaving directory '/work'\n";
    assert_eq!(clean_terminal(text), text);
}

#[test]
fn strips_sgr_colour_and_style() {
    assert_eq!(
        clean_terminal("\u{1b}[1;32m   Compiling\u{1b}[0m foo v0.1.0\n"),
        "   Compiling foo v0.1.0\n"
    );
    // Parameterless SGR is a reset.
    assert_eq!(clean_terminal("\u{1b}[mplain\n"), "plain\n");
}

#[test]
fn keeps_carriage_return_redraws_verbatim() {
    // Rendering these means deciding what the screen held; the overwritten
    // tail is sometimes the message (a crate name and version) and sometimes
    // garbage, and this stage cannot stash a wrong call.
    let progress = "Progress 10%\rProgress 50%\rProgress 100%\n";
    assert_eq!(clean_terminal(progress), progress);
    assert_eq!(clean_terminal("abc\rX\n"), "abc\rX\n");
    let download = "Downloading crate foo v1.2.3 (42%)\r ok\n";
    assert_eq!(clean_terminal(download), download);
}

#[test]
fn keeps_cursor_and_erase_controls_verbatim() {
    // Consuming these without a cursor model would emit text that was never
    // on screen: `abc\r` + CUF(2) + `X` renders as `abX`, not `Xbc`.
    let cursor = "abc\r\u{1b}[2CX\n";
    assert_eq!(clean_terminal(cursor), cursor);
    let erase = "Progress 100%\r ok\u{1b}[K\n";
    assert_eq!(clean_terminal(erase), erase);
}

#[test]
fn keeps_osc_sequences_verbatim() {
    // OSC 8 wraps a hyperlink: stripping the sequence keeps the label and
    // drops the URL, which is information, not decoration.
    let link = "\u{1b}]8;;http://x\u{1b}\\link text\n";
    assert_eq!(clean_terminal(link), link);
    let title = "\u{1b}]0;window title\u{7}real output\n";
    assert_eq!(clean_terminal(title), title);
}

#[test]
fn keeps_spinner_and_progress_glyph_lines() {
    // Braille frames are animation residue and block elements draw both
    // progress bars and bar charts. Both are omitted downstream by the
    // build/log compressor, where the removal is stashed and retrievable.
    let spinner = "\u{280b} \n\u{2819}\nreal output\n";
    assert_eq!(clean_terminal(spinner), spinner);
    let bar = "█████░░░░░\nBuild complete\n";
    assert_eq!(clean_terminal(bar), bar);
}

#[test]
fn keeps_non_sgr_escape_sequences_verbatim() {
    // Charset designation, keypad mode, and a bare trailing ESC are not SGR;
    // this stage removes only what it positively recognizes.
    for text in ["\u{1b}(Bhello\n", "\u{1b}=keypad\n", "tail\u{1b}"] {
        assert_eq!(clean_terminal(text), text);
    }
    // An unterminated CSI is not SGR either — no final `m` ever arrives.
    assert_eq!(clean_terminal("\u{1b}[12\nkept\n"), "\u{1b}[12\nkept\n");
    // A CSI with a non-SGR final byte stays whole.
    assert_eq!(clean_terminal("\u{1b}[2K\nkept\n"), "\u{1b}[2K\nkept\n");
}

#[test]
fn strips_sgr_around_other_escapes() {
    // A stripped SGR next to an untouched sequence must not disturb it.
    assert_eq!(
        clean_terminal("\u{1b}[31m\u{1b}[2Kred\u{1b}[0m\n"),
        "\u{1b}[2Kred\n"
    );
}

#[test]
fn keeps_blank_and_whitespace_only_lines() {
    let text = "a\n\nb\n    \nc\n";
    assert_eq!(clean_terminal(text), text);
}

#[test]
fn last_line_without_newline_is_preserved() {
    assert_eq!(clean_terminal("one\ntwo"), "one\ntwo");
}

#[test]
fn is_idempotent() {
    let messy = "\u{1b}[1mBuild\u{1b}[0m\nstep 1\rstep 2\r\n⠙\n█░░\n\u{1b}]0;t\u{7}tail\nplain\n";
    let once = clean_terminal(messy);
    assert_eq!(clean_terminal(&once), once);
    // Only colour left: everything else survived the first pass.
    assert_eq!(once, "Build\nstep 1\rstep 2\r\n⠙\n█░░\n\u{1b}]0;t\u{7}tail\nplain\n");
}
