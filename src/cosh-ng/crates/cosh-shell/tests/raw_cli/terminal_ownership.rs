use super::support::terminal_screen::TerminalSession;

fn wait_reference(session: &mut TerminalSession, label: &str, expected: &vt100::Parser) {
    session.wait_screen(label, |screen| {
        screen.contents() == expected.screen().contents()
            && screen.cursor_position() == expected.screen().cursor_position()
    });
}

fn assert_returned_prompt(session: &TerminalSession) {
    let contents = session.screen().contents();
    let mut lines = contents.lines().rev();
    assert_eq!(lines.next(), Some("screen$ "), "{contents}");
    assert_eq!(lines.next(), Some("◇ "), "{contents}");
    assert_eq!(session.screen().cursor_position().1, 8, "{contents}");
}

fn ownership_transitions(shell: &str) {
    let mut session = TerminalSession::spawn_for_shell(shell, "enhanced", 80);
    session.send(b"printf '\\033[H\\033[2J'\n");
    let mut expected = vt100::Parser::new(24, 80, 0);
    expected.process(b"\xe2\x97\x87 \r\nscreen$ ");
    wait_reference(&mut session, "initial ownership row", &expected);

    // Each explicit mode change publishes one status, without submitting a
    // command to the Shell or repeatedly appending rows during a redraw.
    session.send(b"\x1b[Z");
    expected.process("\r\x1b[2K◌ \r\nscreen$ ".as_bytes());
    wait_reference(&mut session, "Shell-only status", &expected);
    session.send(b"\x1b[Z");
    expected.process("\r\x1b[2K◇ \r\nscreen$ ".as_bytes());
    wait_reference(&mut session, "Assisted status", &expected);

    session.send(b"/mode\n");
    session.wait_screen("panel returns ownership", |screen| {
        screen.contents().contains("Modes") && screen.contents().ends_with("◇ \nscreen$ ")
    });
    assert_returned_prompt(&session);

    session.send(b"/agent\n");
    session.wait_screen("Composer opened", |screen| {
        screen.contents().contains("Agent Composer")
    });
    session.send(b"owner status probe\r");
    session.wait_screen("Agent returns ownership", |screen| {
        screen.contents().contains("Received shell prompt request:")
            && screen.contents().ends_with("◇ \nscreen$ ")
    });
    assert_returned_prompt(&session);

    session.send(b"printf '%s' \"$PS1\" > \"$HOME/owner-ps1\"\n");
    let receipt = session.home().join("owner-ps1");
    session.wait_screen("original prompt value", |screen| {
        std::fs::read(&receipt).is_ok_and(|bytes| bytes == b"screen$ ")
            && screen.contents().ends_with("◇ \nscreen$ ")
    });
    assert_returned_prompt(&session);
    session.finish();
}

#[test]
fn bash_status_rows_preserve_geometry_across_control_transitions() {
    ownership_transitions("bash");
}

#[test]
fn zsh_status_rows_preserve_geometry_across_control_transitions() {
    ownership_transitions("zsh");
}
