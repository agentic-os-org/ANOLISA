use super::support::terminal_screen::TerminalSession;

fn assert_draft(
    session: &mut TerminalSession,
    label: &str,
    row: u16,
    cols: u16,
    prompt: &str,
    command: &str,
) {
    let expected = format!("{prompt}{command}");
    let width = expected.chars().count() as u16;
    let cursor = (row + width / cols, width % cols);
    session.wait_screen(label, |screen| {
        screen
            .rows(0, cols)
            .skip(usize::from(row))
            .collect::<String>()
            == expected
            && screen.cursor_position() == cursor
    });
}

fn resize_draft(integration: &str, after_panel: bool) {
    let mut session = TerminalSession::spawn(integration, 100);
    session.send(b"r(){ printf '%s' \"$1\" > \"$HOME/resize-result\"; }; printf '__SETUP__\\n'\n");
    session.wait_screen("setup", |screen| screen.contents().contains("__SETUP__\n"));
    session.send(b"printf '\\033[2J\\033[H'\n");
    let prompt = session.prompt();
    let mut expected = vt100::Parser::new(24, 100, 0);
    expected.process(session.published_prompt().as_bytes());
    session.wait_screen("clean initial prompt", |screen| {
        screen.contents() == expected.screen().contents()
    });
    if after_panel {
        session.send(b"/mode\n");
        session.wait_screen("slash panel returned", |screen| {
            screen.contents().contains("mode")
                && screen.contents().trim_end().ends_with(prompt.trim_end())
        });
    }
    let row = session.screen().cursor_position().0;
    let payload = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let command = format!("r '{payload}'");
    session.send(command.as_bytes());
    assert_draft(&mut session, "wide draft", row, 100, &prompt, &command);
    // Both initial editing and Readline's SIGWINCH redraw use the same PS1
    // geometry; the ownership status occupies a separate preceding line.
    session.resize(40);
    assert_draft(&mut session, "narrow draft", row, 40, "screen$ ", &command);
    session.resize(100);
    assert_draft(
        &mut session,
        "wide restored draft",
        row,
        100,
        "screen$ ",
        &command,
    );
    session.send(b"\n");
    session.wait_screen("submitted prompt", |screen| {
        screen.contents().trim_end().ends_with(prompt.trim_end())
    });
    assert_eq!(
        std::fs::read(session.home().join("resize-result")).unwrap(),
        payload.as_bytes()
    );
    eprintln!("execution receipt: exact {} bytes", payload.len());
    session.finish();
}

#[test]
fn bash_native_resize_preserves_unsubmitted_draft() {
    resize_draft("native", false);
}

#[test]
fn bash_enhanced_resize_preserves_unsubmitted_draft() {
    resize_draft("enhanced", false);
}

#[test]
fn bash_enhanced_resize_after_slash_preserves_unsubmitted_draft() {
    resize_draft("enhanced", true);
}
