use super::*;
#[cfg(target_os = "linux")]
use nix::libc;

#[test]
fn raw_cli_isolated_candidate_redraws_add_no_status_lines() {
    let prompt = "isolated-owner$ ";
    let home = temp_shell_home("prompt-owner-isolation-values");
    fs::write(home.join(".bashrc"), format!("PS1='{prompt}'\n")).unwrap();
    let home_str = home.to_string_lossy().to_string();
    for isolated in ["1", "false"] {
        for width in ["74", "80", "200"] {
            let output = run_raw_cli_with_args_env_and_delayed_input(
                "fake",
                &["--shell", "bash"],
                &[
                    ("HOME", &home_str),
                    ("COSH_POC_PS1", prompt),
                    ("COSH_SHELL_INTEGRATION", "enhanced"),
                    ("COSH_SHELL_ISOLATED", isolated),
                    ("COSH_SHELL_STARTUP_BANNER", "0"),
                    ("COSH_SHELL_LANG", "en-US"),
                    ("COSH_SHELL_WIDTH", width),
                ],
                vec![
                    ("你".as_bytes().to_vec(), Duration::from_millis(300)),
                    (b"\x15".to_vec(), Duration::from_millis(300)),
                    (
                        b"?? hold test slow agent\n".to_vec(),
                        Duration::from_millis(300),
                    ),
                    (b"/cancel\n".to_vec(), Duration::from_millis(1_000)),
                    (
                        b"echo ordinary-control-1\n".to_vec(),
                        Duration::from_millis(700),
                    ),
                    (
                        b"echo ordinary-control-2\n".to_vec(),
                        Duration::from_millis(300),
                    ),
                    (b"exit\n".to_vec(), Duration::from_millis(300)),
                ],
            );
            let visible = strip_ansi_escape(&output).replace('\r', "");

            assert!(visible.contains("Agent cancellation requested"), "{output}");
            assert!(visible.contains("ordinary-control-2"), "{output}");
            let prompt_count = count_occurrences(&visible, prompt);
            let expected_prompt_count = if isolated == "1" { 8 } else { 5 };
            assert_eq!(
                prompt_count, expected_prompt_count,
                "{isolated}/{width}: {output}"
            );
            assert!(
                !visible.contains("◇ "),
                "{isolated}/{width}: no Assisted status line may be emitted: {output}"
            );
            assert!(
                !visible.contains("◌ "),
                "{isolated}/{width}: no Shell-only status line may be emitted: {output}"
            );
        }
    }
    let _ = fs::remove_dir_all(home);
}

#[test]
fn raw_cli_native_keeps_custom_bash_prompt_undecorated() {
    let home = temp_shell_home("native-custom-prompt");
    fs::write(home.join(".bashrc"), "PS1='native-owner$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "native"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[("native-owner$", b"exit\n")],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("native-owner$ "), "{output}");
    assert!(!output.contains("◇ "), "{output}");
    assert!(!output.contains("◌ "), "{output}");
}

#[cfg(target_os = "macos")]
#[test]
fn raw_cli_login_probe_infrastructure_failure_keeps_path_resolution() {
    let home = temp_shell_home("login-probe-macos-home");
    let missing = temp_shell_home("login-probe-macos-missing-path");
    fs::write(home.join(".bash_profile"), "PS1='macos-fallback-owner$ '\n")
        .expect("write login profile");
    let path = format!(
        "{}:{}",
        missing.display(),
        std::env::var("PATH").expect("test PATH")
    );
    let home_str = home.display().to_string();

    let output = run_raw_cli_with_args_and_env(
        "fake",
        &["--shell", "bash", "--login"],
        "printf '__MACOS_FALLBACK__%s\\n' \"$(shopt -q login_shell && printf yes || printf no)\"\nexit\n",
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LOGIN_IDENTITY", "1"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&missing);

    assert!(output.contains("__MACOS_FALLBACK__no"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_login_probe_preserves_login_argv0() {
    let home = temp_shell_home("login-probe-argv0");
    let logout_log = home.join("logout-log");
    fs::write(home.join(".bash_profile"), "PS1='r2-login-owner$ '\n").expect("write login profile");
    fs::write(
        home.join(".bash_logout"),
        format!(
            "if [ -n \"${{COSH_R2_CAPABILITY_INJECT+x}}\" ]; then printf 'probe\\n'; else printf 'session\\n'; fi >> '{}'\n",
            logout_log.display()
        ),
    )
    .expect("write logout profile");
    let home_str = home.display().to_string();

    let output = run_raw_cli_with_args_and_env(
        "fake",
        &["--shell", "bash", "--login"],
        "printf '__R2_LOGIN__%s|%s|%s\\n' \"$(shopt -q login_shell && printf yes || printf no)\" \"$0\" \"$(shopt -qo posix && printf on || printf off)\"\nexit\n",
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LOGIN_IDENTITY", "1"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
    );
    let logout_events = fs::read_to_string(&logout_log).expect("read logout events");
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(
        visible.contains("__R2_LOGIN__yes|-bash|off"),
        "capability supervisor must preserve argv0=-bash: {output}"
    );
    assert_eq!(
        logout_events, "session\n",
        "capability discovery must not run the real session's logout hook"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_login_probe_reaps_detached_descendants() {
    for mode in ["normal", "timeout"] {
        let home = temp_shell_home(&format!("login-probe-descendant-home-{mode}"));
        let wrapper_dir = temp_shell_home(&format!("login-probe-descendant-path-{mode}"));
        let helper_state_file = wrapper_dir.join("helper-state");
        let wrapper = wrapper_dir.join("bash");
        write_executable(
            &wrapper,
            &format!(
                "#!/bin/bash\nif [ -n \"${{COSH_R2_CAPABILITY_INJECT+x}}\" ]; then\n  /usr/bin/setsid /bin/sh -c 'read -r pid comm state ppid pgrp sid rest < /proc/self/stat; printf \"%s %s\\n\" \"$pid\" \"$sid\" > \"$1\"; exec /bin/sleep 30' sh '{}' >/dev/null 2>&1 &\n  while [ ! -s '{}' ]; do :; done\n  if [ \"$COSH_TEST_PROBE_MODE\" = timeout ]; then exec /bin/sleep 30; fi\nfi\nexec -a -bash /bin/bash \"$@\"\n",
                helper_state_file.display(),
                helper_state_file.display()
            ),
        );
        let path = format!(
            "{}:{}",
            wrapper_dir.display(),
            std::env::var("PATH").expect("test PATH")
        );
        let home_str = home.display().to_string();

        let output = run_raw_cli_with_args_and_env(
            "fake",
            &["--shell", "bash", "--login"],
            "exit\n",
            &[
                ("HOME", &home_str),
                ("PATH", &path),
                ("COSH_TEST_PROBE_MODE", mode),
                ("COSH_SHELL_INTEGRATION", "enhanced"),
                ("COSH_SHELL_ISOLATED", "0"),
                ("COSH_SHELL_LOGIN_IDENTITY", "1"),
                ("COSH_SHELL_STARTUP_BANNER", "0"),
            ],
        );
        let helper_state = fs::read_to_string(&helper_state_file)
            .unwrap_or_else(|error| panic!("probe helper state missing: {error}; output={output}"));
        let mut fields = helper_state.split_whitespace();
        let helper_pid = fields
            .next()
            .expect("helper PID")
            .parse::<i32>()
            .expect("parse helper PID");
        let helper_sid = fields
            .next()
            .expect("helper SID")
            .parse::<i32>()
            .expect("parse helper SID");
        assert_eq!(
            helper_sid, helper_pid,
            "setsid fixture did not create a detached session"
        );
        let helper_alive = unsafe { libc::kill(helper_pid, 0) == 0 };
        if helper_alive {
            let cmdline = fs::read(format!("/proc/{helper_pid}/cmdline")).unwrap_or_default();
            if cmdline.starts_with(b"/bin/sleep\0") {
                unsafe {
                    libc::kill(helper_pid, libc::SIGKILL);
                }
            }
        }
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&wrapper_dir);

        assert!(
            !helper_alive,
            "{mode} capability probe left detached helper PID {helper_pid} alive; output={output}"
        );
    }
}

#[test]
fn raw_cli_default_enhanced_keeps_bash_prompt_undecorated_without_mutating_ps1() {
    let home = temp_shell_home("enhanced-custom-prompt");
    fs::write(home.join(".bashrc"), "PS1='enhanced-owner$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("enhanced-owner$", b"printf '__PS1__<%s>\\n' \"$PS1\"\n"),
            ("__PS1__<enhanced-owner$ >", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    // The default Enhanced session must render the child prompt exactly as
    // the shell drew it: no status symbol lines, no leftover blank lines.
    assert!(
        !visible.contains("◇ "),
        "no Assisted status line may be emitted: {output}"
    );
    assert!(
        !visible.contains("◌ "),
        "no Shell-only status line may be emitted: {output}"
    );
    // The prompt text itself must appear at least twice (initial prompt and
    // the post-command repaint). Line-start anchoring is not portable: the
    // first prompt may sit at the very start of the output, and in-place
    // redraws (\r + clear-line) do not create new lines. Row geometry is
    // covered by the VT100 terminal_ownership tests.
    assert!(
        count_occurrences(&visible, "enhanced-owner$ ") >= 2,
        "{output}"
    );
    assert!(visible.contains("__PS1__<enhanced-owner$ >"), "{output}");
    assert!(!visible.contains("__PS1__<◇ enhanced-owner$ >"), "{output}");
}

#[test]
fn raw_cli_enhanced_passes_through_user_prompt_containing_status_glyph() {
    let home = temp_shell_home("enhanced-glyph-prompt");
    fs::write(home.join(".bashrc"), "PS1='◇ owner$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("◇ owner$", b"printf '__PS1__<%s>\\n' \"$PS1\"\n"),
            ("__PS1__<◇ owner$ >", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    // The glyph comes from the user's own PS1 and must render verbatim,
    // exactly once per prompt, with no extra injected status line.
    assert!(
        count_occurrences(&visible, "◇ owner$ ") >= 2,
        "user prompt containing ◇ must pass through unchanged: {output}"
    );
    assert!(!visible.contains("◇ \n◇"), "{output}");
    assert!(!visible.contains("◌ "), "{output}");
    assert!(visible.contains("__PS1__<◇ owner$ >"), "{output}");
}

#[test]
fn raw_cli_enhanced_status_symbols_on_decorates_bash_prompt_without_mutating_ps1() {
    let home = temp_shell_home("enhanced-status-symbols-on");
    fs::write(home.join(".bashrc"), "PS1='symbol-owner$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_STATUS_SYMBOLS", "1"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("symbol-owner$", b"printf '__PS1__<%s>\\n' \"$PS1\"\n"),
            ("__PS1__<symbol-owner$ >", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    // The status occupies its own line directly above the prompt; the prompt
    // itself and PS1 stay untouched.
    assert!(
        count_occurrences(&visible, "◇ \nsymbol-owner$ ") >= 2,
        "{output}"
    );
    assert!(!visible.contains("◌ "), "{output}");
    assert!(!visible.contains("◇ ◇"), "{output}");
    assert!(visible.contains("__PS1__<symbol-owner$ >"), "{output}");
    assert!(!visible.contains("__PS1__<◇ symbol-owner$ >"), "{output}");
}

#[test]
fn raw_cli_enhanced_status_symbols_follow_shift_tab_ownership() {
    let home = temp_shell_home("enhanced-status-symbols-toggle");
    fs::write(home.join(".bashrc"), "PS1='toggle-owner$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_STATUS_SYMBOLS", "1"),
        ],
        vec![
            (b"\x1b[Z".to_vec(), Duration::from_millis(500)),
            (b"\x1b[Z".to_vec(), Duration::from_millis(500)),
            (b"exit 0\n".to_vec(), Duration::from_millis(300)),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    // Initial Assisted publish, Shell-only after the first toggle, Assisted
    // again after the second.
    assert!(
        count_occurrences(&visible, "◇ \ntoggle-owner$ ") >= 2,
        "{output}"
    );
    assert!(visible.contains("◌ \ntoggle-owner$ "), "{output}");
}

#[test]
fn raw_cli_status_symbols_config_file_enables_symbols() {
    let home = temp_shell_home("status-symbols-config-file");
    fs::write(home.join(".bashrc"), "PS1='file-owner$ '\n").unwrap();
    write_cosh_config(&home, "shell.status_symbols = true\n");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[("file-owner$", b"exit\n")],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(visible.contains("◇ \nfile-owner$ "), "{output}");
}

#[test]
fn raw_cli_status_symbols_table_form_enables_symbols() {
    let home = temp_shell_home("status-symbols-table-form");
    fs::write(home.join(".bashrc"), "PS1='table-owner$ '\n").unwrap();
    write_cosh_config(&home, "[shell]\nstatus_symbols = true\n");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[("table-owner$", b"exit\n")],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(visible.contains("◇ \ntable-owner$ "), "{output}");
}

#[test]
fn raw_cli_status_symbols_simple_key_invalid_bare_value_stays_off() {
    let home = temp_shell_home("status-symbols-invalid-bare-value");
    fs::write(home.join(".bashrc"), "PS1='bare-invalid-owner$ '\n").unwrap();
    write_cosh_config(&home, "shell.status_symbols = sometimes\n");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[("bare-invalid-owner$", b"exit\n")],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(visible.contains("bare-invalid-owner$ "), "{output}");
    assert!(!visible.contains("◇ "), "{output}");
    assert!(!visible.contains("◌ "), "{output}");
}

#[test]
fn raw_cli_status_symbols_env_overrides_config_file() {
    let home = temp_shell_home("status-symbols-env-overrides-file");
    fs::write(home.join(".bashrc"), "PS1='override-owner$ '\n").unwrap();
    write_cosh_config(&home, "[shell]\nstatus_symbols = true\n");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_STATUS_SYMBOLS", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[("override-owner$", b"exit\n")],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(visible.contains("override-owner$ "), "{output}");
    assert!(!visible.contains("◇ "), "{output}");
    assert!(!visible.contains("◌ "), "{output}");
}

#[test]
fn raw_cli_status_symbols_invalid_config_value_stays_off() {
    let home = temp_shell_home("status-symbols-invalid-value");
    fs::write(home.join(".bashrc"), "PS1='invalid-owner$ '\n").unwrap();
    write_cosh_config(&home, "[shell]\nstatus_symbols = \"sometimes\"\n");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[("invalid-owner$", b"exit\n")],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(visible.contains("invalid-owner$ "), "{output}");
    assert!(!visible.contains("◇ "), "{output}");
    assert!(!visible.contains("◌ "), "{output}");
}

#[test]
fn raw_cli_native_never_shows_status_symbols() {
    let home = temp_shell_home("native-status-symbols-gated");
    fs::write(home.join(".bashrc"), "PS1='native-gated$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "native"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_STATUS_SYMBOLS", "1"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[("native-gated$", b"exit\n")],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("native-gated$ "), "{output}");
    assert!(!output.contains("◇ "), "{output}");
    assert!(!output.contains("◌ "), "{output}");
}

#[test]
fn raw_cli_enhanced_status_symbols_on_decorates_zsh_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("enhanced-status-symbols-on-zsh");
    fs::write(home.join(".zshrc"), "PROMPT='symbol-zsh> '\nRPROMPT=''\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_STATUS_SYMBOLS", "1"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("symbol-zsh> ", b"printf '__PROMPT__<%s>\\n' \"$PROMPT\"\n"),
            ("__PROMPT__<symbol-zsh> >", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(
        count_occurrences(&visible, "◇ \nsymbol-zsh> ") >= 2,
        "{output}"
    );
    assert!(visible.contains("__PROMPT__<symbol-zsh> >"), "{output}");
    assert!(!visible.contains("__PROMPT__<◇ symbol-zsh> >"), "{output}");
}

#[test]
fn raw_cli_mode_routing_switches_the_live_enhanced_session() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        vec![
            (
                b"/mode routing shell-only\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"hello there\n".to_vec(), Duration::from_millis(300)),
            (
                b"/mode routing assisted\n".to_vec(),
                Duration::from_millis(300),
            ),
            (b"hello there\n".to_vec(), Duration::from_millis(300)),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(
        visible.contains("Routing mode set to shell-only."),
        "{output}"
    );
    assert!(
        visible.contains("Routing mode set to assisted."),
        "{output}"
    );
    assert_eq!(
        count_occurrences(&visible, "hello: command not found"),
        1,
        "{output}"
    );
    assert!(
        !visible.contains("◌ ") && !visible.contains("◇ "),
        "routing switches must not emit status symbol lines: {output}"
    );
}

#[test]
fn raw_cli_enhanced_keeps_zsh_prompt_undecorated_without_mutating_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("enhanced-custom-zsh-prompt");
    fs::write(home.join(".zshrc"), "PROMPT='enhanced-zsh> '\nRPROMPT=''\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            (
                "enhanced-zsh> ",
                b"printf '__PROMPT__<%s>\\n' \"$PROMPT\"\n",
            ),
            ("__PROMPT__<enhanced-zsh> >", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(
        count_occurrences(&visible, "enhanced-zsh> ") >= 2,
        "{output}"
    );
    assert!(
        !visible.contains("◇ ") && !visible.contains("◌ "),
        "no status symbol lines may be emitted: {output}"
    );
    assert!(visible.contains("__PROMPT__<enhanced-zsh> >"), "{output}");
}

#[test]
fn raw_cli_enhanced_shift_tab_toggles_zsh_routing_in_place() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("enhanced-zsh-toggle");
    fs::write(home.join(".zshrc"), "PROMPT='enhanced-zsh> '\nRPROMPT=''\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("enhanced-zsh> ", b"\x1b[Z"),
            ("enhanced-zsh> ", b"/help\n"),
            ("no such file or directory: /help", b""),
            ("enhanced-zsh> ", b"\x1b[Z"),
            ("enhanced-zsh> ", b"exit 0\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output).replace('\r', "");

    assert!(
        visible.contains("no such file or directory: /help"),
        "{output}"
    );
    assert!(
        count_occurrences(&visible, "enhanced-zsh> ") >= 2,
        "{output}"
    );
    assert!(
        !visible.contains("◇ ") && !visible.contains("◌ "),
        "Shift+Tab toggles must not emit status symbol lines: {output}"
    );
}

#[test]
fn raw_cli_explicit_native_skips_enhanced_startup_workers() {
    let home = temp_shell_home("native-no-enhanced-workers");
    let core = home.join("cosh-core-probe");
    let invocation_log = home.join("core-invoked");
    write_executable(
        &core,
        &format!(
            "#!/bin/sh\nprintf invoked >> '{}'\nexit 1\n",
            invocation_log.display()
        ),
    );
    let home_str = home.to_string_lossy().into_owned();
    let core_str = core.to_string_lossy().into_owned();

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("COSH_CORE_PATH", core_str.as_str()),
            ("COSH_SHELL_INTEGRATION", "native"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_RECOMMENDATIONS_ENABLED", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
        ],
        vec![(b"exit\n".to_vec(), Duration::from_millis(300))],
    );

    assert!(!invocation_log.exists(), "{output}");
    assert!(
        !home.join(".copilot-shell/cosh/recommendations").exists(),
        "{output}"
    );
}

#[test]
fn raw_cli_zsh_native_loads_existing_user_history() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("native-history");
    let history_file = home.join(".zsh_history");
    fs::write(
        home.join(".zshenv"),
        "export HISTFILE=$HOME/.zsh_history\nHISTSIZE=1000\nSAVEHIST=1000\nfc -R \"$HISTFILE\" 2>/dev/null || true\n",
    )
    .unwrap();
    fs::write(
        home.join(".zshrc"),
        "setopt appendhistory incappendhistory\n",
    )
    .unwrap();
    fs::write(&history_file, "echo old-cosh-zsh-history\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let history_str = history_file.to_string_lossy().to_string();

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_INTEGRATION", "native"),
        ],
        vec![
            (
                b"printf 'histfile:%s\\n' \"$HISTFILE\"\n".to_vec(),
                Duration::ZERO,
            ),
            (b"history\n".to_vec(), Duration::from_millis(150)),
            (
                b"echo new-cosh-zsh-history\n".to_vec(),
                Duration::from_millis(150),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(150)),
        ],
    );

    assert!(
        output.contains(&format!("histfile:{history_str}")),
        "{output}"
    );
    assert!(output.contains("old-cosh-zsh-history"), "{output}");
    assert!(fs::read_to_string(&history_file)
        .unwrap()
        .contains("new-cosh-zsh-history"));
}

#[test]
fn raw_cli_bash_native_loads_existing_user_history() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let home = temp_shell_home("native-bash-history");
    let history_file = home.join(".bash_history");
    fs::write(
        home.join(".bashrc"),
        "export HISTFILE=$HOME/.bash_history\nexport HISTSIZE=1000\nshopt -s histappend\n",
    )
    .unwrap();
    fs::write(&history_file, "echo old-cosh-bash-history\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let history_str = history_file.to_string_lossy().to_string();

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_INTEGRATION", "native"),
        ],
        vec![
            (
                b"printf 'histfile:%s\\n' \"$HISTFILE\"\n".to_vec(),
                Duration::ZERO,
            ),
            (b"history\n".to_vec(), Duration::from_millis(150)),
            (
                b"echo new-cosh-bash-history\n".to_vec(),
                Duration::from_millis(150),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(150)),
        ],
    );

    assert!(
        output.contains(&format!("histfile:{history_str}")),
        "{output}"
    );
    assert!(output.contains("old-cosh-bash-history"), "{output}");
    assert!(fs::read_to_string(&history_file)
        .unwrap()
        .contains("new-cosh-bash-history"));
}

#[test]
#[ignore = "native zsh completion can invoke user rc and real editor; keep out of default raw_cli"]
fn raw_cli_zsh_native_path_slash_and_tab_stay_in_shell() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_INTEGRATION", "native"),
        ],
        vec![
            (b"/Users".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(100)),
            (b"vim .".to_vec(), Duration::from_millis(100)),
            (b"/".to_vec(), Duration::from_millis(50)),
            (b"\t".to_vec(), Duration::from_millis(50)),
            (vec![0x03], Duration::from_millis(100)),
            (
                b"echo after-native-tab\n".to_vec(),
                Duration::from_millis(100),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("after-native-tab"), "{output}");
    assert!(!output.contains("Slash command hint"), "{output}");
    assert!(!output.contains("Slash commands"), "{output}");
    assert!(!output.contains("User mode"), "{output}");
    assert!(!output.contains("/mode [recommend|agent]"), "{output}");
}
