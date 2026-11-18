use super::*;

// V02 boundary evidence, not an anti-forgery pass. Design section 7.3 places the
// startup script and any code running in the same Shell inside one trust domain:
// the readonly marker token is still readable there, so a valid precmd can be
// forged. These fixtures record what actually happens to the result projection,
// the dispatch gate and the ownership of the next action's input. The oracle is
// the event stream plus the real terminal bytes and the real foreground
// behaviour, never the marker itself.

/// A foreground action that keeps running after it prints a precmd marker.
/// `read -t 2` bounds the wait so the controls finish on their own instead of
/// depending on the harness gate timeout.
fn foreground_action(injection: &str) -> String {
    format!(
        "{injection}read -r -t 2 v02_consumed ; printf 'V02-READ-CONSUMED:%s\\n' \"$v02_consumed\""
    )
}

/// The `toke""n` split keeps the literal line out of the marker script's
/// secret-redaction patterns while bash still prints a well-formed key.
fn precmd_injection(token_expr: &str, cwd: &str) -> String {
    format!(
        "printf \"\\033]1337;COSH;{{\\\"event\\\":\\\"precmd\\\",\\\"toke\"\"n\\\":\\\"%s\\\",\\\"cwd\\\":\\\"{cwd}\\\",\\\"status\\\":0}}\\a\" \"{token_expr}\" ; "
    )
}

/// The same shape with the token key omitted entirely: a marker that cannot be
/// correlated with the session at all.
fn precmd_injection_without_token(cwd: &str) -> String {
    format!(
        "printf \"\\033]1337;COSH;{{\\\"event\\\":\\\"precmd\\\",\\\"cwd\\\":\\\"{cwd}\\\",\\\"status\\\":0}}\\a\" ; "
    )
}

fn v02_config(label: &str) -> (ShellHostConfig, std::path::PathBuf) {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-v02-{label}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let mut config = ShellHostConfig::new(label, &work_dir);
    config.native_mode = true;
    (config, work_dir)
}

fn finished<'a>(output: &'a ShellHostOutput, needle: &str) -> Option<&'a ShellEvent> {
    output.events.iter().find(|event| {
        matches!(
            event.kind,
            ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
        ) && event
            .command
            .as_deref()
            .is_some_and(|command| command.contains(needle))
    })
}

fn started(output: &ShellHostOutput, command: &str) -> bool {
    output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted && event.command.as_deref() == Some(command)
    })
}

#[test]
fn shell_host_pty_valid_forged_precmd_releases_the_gate_while_the_foreground_runs() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    let (config, work_dir) = v02_config("valid");
    let nonce = unique_suffix();
    let forged_cwd = format!("/tmp/v02-forged-{nonce}");
    let next = format!("echo V02-B-{nonce}");
    let action = foreground_action(&precmd_injection("$_COSH_MARKER_TOKEN", &forged_cwd));
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(
                "printf 'V02-TOKEN-READABLE=%s\\n' \"$([ -n \"$_COSH_MARKER_TOKEN\" ] && echo yes || echo no)\"",
            ),
            ScriptedInput::user_line(action),
            ScriptedInput::user_line(next.clone()),
        ],
    )
    .expect("scripted bash pty");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    // The premise of the whole boundary: same-Shell code can read the readonly
    // private token, so the forged marker carries the session's real token. The
    // token length differs across platforms, so the oracle is readability.
    assert!(terminal.contains("V02-TOKEN-READABLE=yes"), "{terminal}");

    // Result projection: the forged precmd is accepted as the action's terminal
    // fact, including the attacker-chosen end_cwd and exit status.
    let projected = finished(&output, "v02_consumed").expect("action finish event");
    assert_eq!(projected.kind, ShellEventKind::CommandCompleted);
    assert_eq!(projected.exit_code, Some(0));
    assert_eq!(projected.end_cwd.as_deref(), Some(forged_cwd.as_str()));

    // Input ownership: the next action's bytes were consumed by the still
    // running foreground read, which also proves the gate released before the
    // action really finished. The next action never ran as a command.
    assert!(
        terminal.contains(&format!("V02-READ-CONSUMED:{next}")),
        "{terminal}"
    );
    assert!(!started(&output, &next), "{:?}", output.events);
    assert!(
        !terminal.contains(&format!("\nV02-B-{nonce}\n")),
        "{terminal}"
    );

    assert_no_osc_marker(&output.terminal_output);
    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn shell_host_pty_wrong_token_precmd_does_not_project_a_forged_terminal_fact() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    let (config, work_dir) = v02_config("wrong-token");
    let nonce = unique_suffix();
    let forged_cwd = format!("/tmp/v02-forged-{nonce}");
    let next = format!("echo V02-B-{nonce}");
    let action = foreground_action(&precmd_injection("wrong-token", &forged_cwd));
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(action),
            ScriptedInput::user_line(next.clone()),
        ],
    )
    .expect("scripted bash pty");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    // A mismatched token is noise: the action finishes through the genuine
    // marker chain with its real directory, not the forged one.
    let real = finished(&output, "v02_consumed").expect("action finish event");
    assert_eq!(real.kind, ShellEventKind::CommandCompleted);
    assert_ne!(real.end_cwd.as_deref(), Some(forged_cwd.as_str()));

    // The foreground ended on its own read timeout before the next line was
    // sent, so the next action really ran instead of being swallowed.
    assert!(started(&output, &next), "{:?}", output.events);
    assert!(terminal.contains(&format!("V02-B-{nonce}")), "{terminal}");
    assert!(
        !terminal.contains(&format!("V02-READ-CONSUMED:{next}")),
        "{terminal}"
    );

    assert_no_osc_marker(&output.terminal_output);
    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn shell_host_pty_plain_foreground_without_a_marker_keeps_the_next_action() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    let (config, work_dir) = v02_config("plain");
    let nonce = unique_suffix();
    let next = format!("echo V02-B-{nonce}");
    let action = foreground_action("");
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(action),
            ScriptedInput::user_line(next.clone()),
        ],
    )
    .expect("scripted bash pty");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    // Control: without any injected marker the same foreground shape does not
    // produce a forged terminal fact, and the next action still runs.
    let real = finished(&output, "v02_consumed").expect("action finish event");
    assert_eq!(real.kind, ShellEventKind::CommandCompleted);
    assert!(started(&output, &next), "{:?}", output.events);
    assert!(terminal.contains(&format!("V02-B-{nonce}")), "{terminal}");
    assert!(
        !terminal.contains(&format!("V02-READ-CONSUMED:{next}")),
        "{terminal}"
    );

    assert_no_osc_marker(&output.terminal_output);
    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn shell_host_pty_tokenless_precmd_does_not_project_a_forged_terminal_fact() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    let (config, work_dir) = v02_config("tokenless");
    let nonce = unique_suffix();
    let forged_cwd = format!("/tmp/v02-forged-{nonce}");
    let next = format!("echo V02-B-{nonce}");
    let action = foreground_action(&precmd_injection_without_token(&forged_cwd));
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(action),
            ScriptedInput::user_line(next.clone()),
        ],
    )
    .expect("scripted bash pty");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    let real = finished(&output, "v02_consumed").expect("action finish event");
    assert_eq!(real.kind, ShellEventKind::CommandCompleted);
    assert_ne!(real.end_cwd.as_deref(), Some(forged_cwd.as_str()));
    assert!(started(&output, &next), "{:?}", output.events);
    assert!(terminal.contains(&format!("V02-B-{nonce}")), "{terminal}");
    assert!(
        !terminal.contains(&format!("V02-READ-CONSUMED:{next}")),
        "{terminal}"
    );

    assert_no_osc_marker(&output.terminal_output);
    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn shell_host_pty_background_output_is_preserved_without_a_command_projection() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    let (config, work_dir) = v02_config("background");
    let nonce = unique_suffix();
    let background = format!("( sleep 1 ; printf 'V02-BG-{nonce}\\n' ) &");
    let foreground = format!("echo V02-FG-{nonce}");
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(background),
            ScriptedInput::user_line(foreground.clone()),
            ScriptedInput::user_line("sleep 2"),
        ],
    )
    .expect("scripted bash pty");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    // Normal background output is preserved rather than dropped, and it does
    // not become a command of its own. Coverage of these bytes in the shared
    // PTY window does not prove they were produced by the foreground action,
    // so no exclusive-causality claim is made here.
    assert!(terminal.contains(&format!("V02-BG-{nonce}")), "{terminal}");
    assert!(terminal.contains(&format!("V02-FG-{nonce}")), "{terminal}");
    assert!(!started(&output, &format!("printf 'V02-BG-{nonce}\\n'")));
    let real = finished(&output, &foreground).expect("foreground finish event");
    assert_eq!(real.kind, ShellEventKind::CommandCompleted);
    assert_eq!(real.exit_code, Some(0));

    let ledger = ledger_from_output(&output);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert_no_osc_marker(&output.terminal_output);
    let _ = std::fs::remove_dir_all(&work_dir);
}
