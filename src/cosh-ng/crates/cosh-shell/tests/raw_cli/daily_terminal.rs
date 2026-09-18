use super::support::terminal_screen::TerminalSession;

fn startup(shell: &str) {
    let rc = if shell == "bash" { ".bashrc" } else { ".zshrc" };
    for integration in ["native", "enhanced"] {
        let mut session = TerminalSession::spawn_with_startup(
            shell,
            integration,
            100,
            &[
                (rc, "printf 'rc:' >> \"$HOME/startup-trace\"\nsource \"$HOME/user-env\"\nPS1='screen$ '\n"),
                ("user-env", "printf 'source' >> \"$HOME/startup-trace\"\nexport DAILY_VALUE='user value'\n"),
                (".bash_profile", "printf 'login' >> \"$HOME/startup-trace\"\n"),
                (".zprofile", "printf 'login' >> \"$HOME/startup-trace\"\n"),
                (".zlogin", "printf 'login' >> \"$HOME/startup-trace\"\n"),
            ],
        );
        let trace = session.home().join("startup-trace");
        assert_eq!(
            std::fs::read(&trace).unwrap(),
            b"rc:source",
            "{shell}/{integration}"
        );
        let receipt = session.home().join("child-env");
        session.send(b"sh -c 'printf %s \"$DAILY_VALUE\"' > \"$HOME/child-env\"\n");
        session.wait_screen("startup environment reaches child", |screen| {
            std::fs::read(&receipt).is_ok_and(|bytes| bytes == b"user value")
                && screen.contents().ends_with("screen$ ")
        });
        assert_eq!(
            std::fs::read(&trace).unwrap(),
            b"rc:source",
            "startup ran again"
        );
        session.finish();
    }
}

fn editing(shell: &str) {
    for integration in ["native", "enhanced"] {
        let mut session = TerminalSession::spawn_for_shell(shell, integration, 100);
        let erased = session.home().join("erased");
        let receipt = session.home().join("paste-result");
        session.send(b"touch \"$HOME/erased\"");
        session.wait_screen("draft before Ctrl-U", |screen| {
            screen
                .contents()
                .ends_with("screen$ touch \"$HOME/erased\"")
        });
        session.send(b"\x15");
        session.wait_screen("Ctrl-U clears draft", |screen| {
            let (row, col) = screen.cursor_position();
            // ZLE erases with literal spaces; Readline can use erase-to-EOL.
            // Both must leave the exact prompt and only blank cells after it.
            col == 8
                && (0..100).all(|column| {
                    let cell = screen.cell(row, column).expect("draft row cell");
                    let expected = "screen$ ".chars().nth(column as usize).unwrap_or(' ');
                    let actual = if cell.has_contents() {
                        cell.contents()
                    } else {
                        " "
                    };
                    actual == expected.to_string() && !cell.is_wide()
                })
        });
        // The embedded newline must remain editable until an explicit Enter.
        session.send(
            "\x1b[200~printf '%s' '中文\nsecond' > \"$HOME/paste-result\"\x1b[201~".as_bytes(),
        );
        session.wait_screen("bracketed paste remains a draft", |screen| {
            screen.contents().contains("中文")
                && screen
                    .contents()
                    .contains("second' > \"$HOME/paste-result\"")
        });
        assert!(
            !erased.exists(),
            "erased draft executed: {shell}/{integration}"
        );
        assert!(
            !receipt.exists(),
            "paste executed before Enter: {shell}/{integration}"
        );
        session.send(b"\r");
        session.wait_screen("explicit Enter submits exact pasted bytes", |screen| {
            std::fs::read(&receipt).is_ok_and(|bytes| bytes == "中文\nsecond".as_bytes())
                && screen.contents().ends_with("screen$ ")
        });
        assert!(
            !erased.exists(),
            "erased draft executed: {shell}/{integration}"
        );
        session.finish();
    }
}

fn exit(shell: &str, input: &[u8], code: i32) {
    for integration in ["native", "enhanced"] {
        let session = TerminalSession::spawn_for_shell(shell, integration, 100);
        session.finish_with_input(input, code);
    }
}

#[test]
fn bash_non_login_startup_sources_user_environment_once() {
    startup("bash");
}

#[test]
fn zsh_non_login_startup_sources_user_environment_once() {
    startup("zsh");
}

#[test]
fn bash_ctrl_u_and_paste_wait_for_explicit_enter() {
    editing("bash");
}

#[test]
fn zsh_ctrl_u_and_paste_wait_for_explicit_enter() {
    editing("zsh");
}

#[test]
fn bash_nonzero_exit_restores_parent_terminal() {
    exit("bash", b"exit 23\n", 23);
}

#[test]
fn zsh_nonzero_exit_restores_parent_terminal() {
    exit("zsh", b"exit 23\n", 23);
}

#[test]
fn bash_eof_restores_parent_terminal() {
    exit("bash", b"\x04", 0);
}

#[test]
fn zsh_eof_restores_parent_terminal() {
    exit("zsh", b"\x04", 0);
}
