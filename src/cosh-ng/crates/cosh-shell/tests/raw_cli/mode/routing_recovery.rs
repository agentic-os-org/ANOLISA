use crate::support::terminal_screen::TerminalSession;

const PROMPT: &str = "original@bash:~$ ";

fn idle_prompt(screen: &vt100::Screen) -> bool {
    let (row, col) = screen.cursor_position();
    col == PROMPT.len() as u16
        && screen.rows(0, 100).nth(row as usize).as_deref() == Some(PROMPT)
        && screen.cell(row, 0).unwrap().fgcolor() == vt100::Color::Idx(2)
}

fn command(session: &mut TerminalSession, input: &[u8], expected: &str) {
    session.send(input);
    session.wait_screen(expected, |screen| {
        screen.contents().contains(expected) && idle_prompt(screen)
    });
}

fn native_session() -> TerminalSession {
    let mut session = TerminalSession::spawn_for_shell("bash", "enhanced", 100);
    session.send(b"PS1='\\[\\e[32m\\]original@bash:\\w$\\[\\e[0m\\] '\n");
    session.wait_screen("original colored PS1", idle_prompt);
    session
}

fn restore_after_readline_input(history: bool) {
    let mut session = native_session();
    command(
        &mut session,
        b"/mode routing shell-only\n",
        "Routing mode set to shell-only.",
    );
    command(
        &mut session,
        b"/mode routing invali\x01\x05d\n",
        "Unknown routing mode: invalid",
    );
    command(
        &mut session,
        b"/mode routin\x01\x05g\n",
        "Current: shell-only",
    );
    if history {
        command(
            &mut session,
            b"history -s '/mode routing assisted'\n",
            "Current: shell-only",
        );
        command(&mut session, b"\x1b[A\n", "Routing mode set to assisted.");
    } else {
        command(
            &mut session,
            b"/mode routing assiste\x01\x05d\n",
            "Routing mode set to assisted.",
        );
    }
    let output = session.raw_output();
    assert!(
        !output.contains("/mode: No such file or directory"),
        "{output}"
    );
    assert_eq!(output.matches("Routing mode set to assisted.").count(), 1);
    session.send(b"printf '%s' \"$PS1\" > original-ps1\n");
    let ps1 = session.home().join("original-ps1");
    session.wait_screen("PS1 value unchanged", |screen| {
        std::fs::read(&ps1).is_ok_and(|value| value == br"\[\e[32m\]original@bash:\w$\[\e[0m\] ")
            && idle_prompt(screen)
    });
    session.finish();
}

#[test]
fn disabled_routing_history_restore_preserves_original_idle_ps1() {
    restore_after_readline_input(true);
}

#[test]
fn disabled_routing_edited_restore_preserves_original_idle_ps1() {
    restore_after_readline_input(false);
}

#[test]
fn disabled_readline_routing_does_not_enable_other_slash_controls() {
    let mut session = native_session();
    command(
        &mut session,
        b"/mode routing shell-only\n",
        "Routing mode set to shell-only.",
    );
    for input in [
        b"/mode approval auto\n".as_slice(),
        b"/mode routing-extra\n",
        b"/mode routing; true\n",
    ] {
        command(&mut session, input, "/mode: No such file or directory");
    }
    command(&mut session, b"/help\n", "/help: No such file or directory");
    command(
        &mut session,
        b"/mode routin\x01\x05g\n",
        "Current: shell-only",
    );
    session.finish();
}

#[test]
fn enabled_native_history_and_edit_keep_bash_ownership_and_original_ps1() {
    let mut session = native_session();
    command(
        &mut session,
        b"history -s '/mode approval recommend'\n",
        PROMPT,
    );
    command(&mut session, b"\x1b[A\n", "Mode set to recommend.");
    command(
        &mut session,
        b"/mode approval aut\x01\x05o\n",
        "Mode set to auto.",
    );
    command(
        &mut session,
        b"echo ordinary-edit-o\x01\x05k\n",
        "ordinary-edit-ok",
    );
    command(
        &mut session,
        b"history -s 'echo ordinary-history-ok'\n",
        PROMPT,
    );
    command(&mut session, b"\x1b[A\n", "ordinary-history-ok");
    assert!(!session.raw_output().contains("No such file or directory"));
    session.finish();
}
