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
    assert!(
        !contents.contains("◇ ") && !contents.contains("◌ "),
        "control returns must not add status symbol lines: {contents}"
    );
    assert_eq!(session.screen().cursor_position().1, 8, "{contents}");
}

fn ownership_transitions(shell: &str) {
    let mut session = TerminalSession::spawn_for_shell(shell, "enhanced", 80);
    session.send(b"printf '\\033[H\\033[2J'\n");
    let mut expected = vt100::Parser::new(24, 80, 0);
    expected.process(b"screen$ ");
    wait_reference(&mut session, "initial prompt", &expected);

    // Each explicit mode change repaints the prompt in place, without
    // submitting a command to the Shell or appending any status row.
    session.send(b"\x1b[Z");
    expected.process("\r\x1b[2Kscreen$ ".as_bytes());
    wait_reference(&mut session, "Shell-only prompt repaint", &expected);
    session.send(b"\x1b[Z");
    expected.process("\r\x1b[2Kscreen$ ".as_bytes());
    wait_reference(&mut session, "Assisted prompt repaint", &expected);

    session.send(b"/mode\n");
    session.wait_screen("panel returns ownership", |screen| {
        screen.contents().contains("Modes") && screen.contents().ends_with("screen$ ")
    });
    assert_returned_prompt(&session);

    session.send(b"/agent\n");
    session.wait_screen("Composer opened", |screen| {
        screen.contents().contains("Agent Composer")
    });
    session.send(b"owner status probe\r");
    session.wait_screen("Agent returns ownership", |screen| {
        screen.contents().contains("Received shell prompt request:")
            && screen.contents().ends_with("screen$ ")
    });
    assert_returned_prompt(&session);

    session.send(b"printf '%s' \"$PS1\" > \"$HOME/owner-ps1\"\n");
    let receipt = session.home().join("owner-ps1");
    session.wait_screen("original prompt value", |screen| {
        std::fs::read(&receipt).is_ok_and(|bytes| bytes == b"screen$ ")
            && screen.contents().ends_with("screen$ ")
    });
    assert_returned_prompt(&session);
    session.finish();
}

#[test]
fn bash_prompt_geometry_survives_control_transitions_without_status_rows() {
    ownership_transitions("bash");
}

#[test]
fn zsh_prompt_geometry_survives_control_transitions_without_status_rows() {
    if std::process::Command::new("zsh")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    ownership_transitions("zsh");
}

fn assert_returned_prompt_with_status(session: &TerminalSession) {
    let contents = session.screen().contents();
    let mut lines = contents.lines().rev();
    assert_eq!(lines.next(), Some("screen$ "), "{contents}");
    assert_eq!(lines.next(), Some("◇ "), "{contents}");
    assert_eq!(session.screen().cursor_position().1, 8, "{contents}");
}

fn ownership_transitions_with_status(shell: &str) {
    let mut session = TerminalSession::spawn_for_shell_with_env(
        shell,
        "enhanced",
        80,
        &[("COSH_SHELL_STATUS_SYMBOLS", "1")],
    );
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
    assert_returned_prompt_with_status(&session);

    session.send(b"/agent\n");
    session.wait_screen("Composer opened", |screen| {
        screen.contents().contains("Agent Composer")
    });
    session.send(b"owner status probe\r");
    session.wait_screen("Agent returns ownership", |screen| {
        screen.contents().contains("Received shell prompt request:")
            && screen.contents().ends_with("◇ \nscreen$ ")
    });
    assert_returned_prompt_with_status(&session);
    session.finish();
}

#[test]
fn bash_status_rows_preserve_geometry_across_control_transitions() {
    ownership_transitions_with_status("bash");
}

#[test]
fn zsh_status_rows_preserve_geometry_across_control_transitions() {
    if std::process::Command::new("zsh")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    ownership_transitions_with_status("zsh");
}
