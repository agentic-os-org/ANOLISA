use super::*;

#[test]
fn raw_cli_routes_slash_bearing_han_prompt_before_shell_execution() {
    let prompt = "你读一下，并安装这个skill：/nonexistent-cosh-2913/SKILL.md";
    for (shell, args) in [("bash", vec![]), ("zsh", vec!["--shell", "zsh"])] {
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let home = temp_shell_home("slash-bearing-han-prompt");
        if shell == "zsh" {
            // Path routing requires a stable namespace, without a line-init hook.
            fs::write(
                home.join(".zshrc"),
                "zle -D zle-line-init 2>/dev/null || true\nPROMPT='cosh-osc$ '\n",
            )
            .unwrap();
        }
        let home_str = home.to_string_lossy().to_string();
        let output = run_raw_cli_with_args_env_and_delayed_input(
            "fake",
            &args,
            &[
                ("COSH_SHELL_INTEGRATION", "enhanced"),
                ("COSH_SHELL_STARTUP_BANNER", "0"),
                ("HOME", &home_str),
                (
                    "COSH_SHELL_ISOLATED",
                    if shell == "zsh" { "0" } else { "1" },
                ),
                ("LANG", "C.UTF-8"),
                ("LC_ALL", "C.UTF-8"),
            ],
            vec![
                (prompt.as_bytes().to_vec(), Duration::from_millis(300)),
                (b"\n".to_vec(), Duration::from_millis(50)),
                (
                    b"echo after-path-prompt\n".to_vec(),
                    Duration::from_millis(500),
                ),
                (b"exit\n".to_vec(), Duration::from_millis(100)),
            ],
        );
        let _ = fs::remove_dir_all(&home);

        assert!(
            output.contains(&format!("Received shell prompt request: {prompt}")),
            "{shell}: {output}"
        );
        assert!(
            output.contains(&format!("Agent input: {prompt}")),
            "{shell}: {output}"
        );
        assert!(output.contains("after-path-prompt"), "{shell}: {output}");
        assert!(
            !output.contains("No such file or directory")
                && !output.contains("no such file or directory"),
            "{shell}: {output}"
        );
        assert!(!output.contains("command not found"), "{shell}: {output}");
    }
}

#[test]
fn raw_cli_redacts_cjk_adjacent_secret_in_path_prompt_notice() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz123456";
    let prompt = format!("打开./nonexistent-cosh-2913，token是{secret}的内容");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (prompt.as_bytes().to_vec(), Duration::from_millis(300)),
            (b"\n".to_vec(), Duration::from_millis(50)),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(
        output.contains("Received shell prompt request:"),
        "{output}"
    );
    assert!(output.contains("Agent input: <redacted>"), "{output}");
    assert!(
        !output.contains(&format!("Agent input: {prompt}")),
        "{output}"
    );
}

#[test]
fn raw_cli_routes_slash_leading_path_prompt_in_one_input_batch() {
    let prompt = "/nonexistent-cosh-2913/SKILL.md 帮我读一下";
    let mut submission = prompt.as_bytes().to_vec();
    submission.push(b'\n');
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (submission, Duration::from_millis(300)),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(
        output.contains(&format!("Received shell prompt request: {prompt}")),
        "{output}"
    );
    assert!(!output.contains("AI request"), "{output}");
    assert!(!output.contains("No such file or directory"), "{output}");
}

#[test]
fn raw_cli_zsh_routes_space_prefixed_path_prompt_before_zle() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("space-prefixed-path-prompt");
    // This routing case requires no line-init hook, including distro defaults.
    fs::write(
        home.join(".zshrc"),
        "zle -D zle-line-init 2>/dev/null || true\n\
         PROMPT='zsh-space-path> '\n\
         RPROMPT=''\n\
         _cosh_test_accept_line() {\n\
           [[ \"$BUFFER\" == *nonexistent-cosh-space-prefix* ]] && print -r -- space-path-reached-zle\n\
           zle .accept-line\n\
         }\n\
         _cosh_test_tab_after_agent_submit() {\n\
           print -r -- tab-after-agent-submit-reached-zle\n\
           zle .redisplay\n\
         }\n\
         zle -N accept-line _cosh_test_accept_line\n\
         zle -N _cosh_test_tab_after_agent_submit\n\
         bindkey '^I' _cosh_test_tab_after_agent_submit\n",
    )
    .unwrap();
    let home_str = home.to_string_lossy().to_string();
    let prompt = "  /nonexistent-cosh-space-prefix/SKILL.md 帮我读一下";
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (b" ".to_vec(), Duration::from_millis(300)),
            (prompt.as_bytes()[1..].to_vec(), Duration::from_millis(50)),
            (b"\n\t".to_vec(), Duration::from_millis(50)),
            (
                b"echo after-space-path\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains(&format!("Received shell prompt request: {prompt}")),
        "{output}"
    );
    assert!(output.contains("after-space-path"), "{output}");
    assert!(!output.contains("space-path-reached-zle"), "{output}");
    assert!(
        !output.contains("tab-after-agent-submit-reached-zle"),
        "{output}"
    );
    assert!(!output.contains("no such file or directory"), "{output}");
}

#[test]
fn raw_cli_zsh_routes_fragmented_slash_leading_path_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("path-prompt-custom-kill-line");
    // This routing case requires no line-init hook, including distro defaults.
    fs::write(
        home.join(".zshrc"),
        "zle -D zle-line-init 2>/dev/null || true\n\
         {\n\
           request_fd=${_COSH_PATH_PROMPT_REQUEST_FD:-${COSH_ZSH_PATH_PROMPT_REQUEST_FD:-}}\n\
           acknowledgment_fd=${_COSH_PATH_PROMPT_ACK_FD:-${COSH_ZSH_PATH_PROMPT_ACK_FD:-}}\n\
           if [[ -n \"$request_fd\" && -n \"$acknowledgment_fd\" ]]; then\n\
             print -r -- path-prompt-fork-channel-exposed\n\
             IFS= read -r -u \"$request_fd\" request && print -r -- accepted >&\"$acknowledgment_fd\"\n\
           fi\n\
         } &!\n\
         PROMPT='zsh-path:%?> '\nRPROMPT=''\nbindkey -e\nbindkey '^U' self-insert\n\
         _cosh_test_private_submit() { print -r -- path-prompt-private-binding-ran; zle .redisplay; }\n\
         zle -N _cosh_test_private_submit\n\
         bindkey $'\\e[99;99u' _cosh_test_private_submit\n\
         TRAPINT() { print -r -- path-prompt-user-trap-ran; return 130; }\n\
         TRAPURG() { print -r -- path-prompt-user-urg-trap-ran; return 0; }\n\
         typeset -gi COSH_TEST_PRECMD_COUNT=0\n\
         _cosh_test_slow_third_precmd() {\n\
           (( ++COSH_TEST_PRECMD_COUNT ))\n\
           (( COSH_TEST_PRECMD_COUNT == 3 )) && sleep 3\n\
           return 0\n\
         }\n\
         precmd_functions+=(_cosh_test_slow_third_precmd)\n\
         typeset -g COSH_TEST_PATH_PROMPT='/nonexistent-cosh-1943/SKILL.md 帮我读一下'\n\
         typeset -g COSH_TEST_HISTORY_LEAK='path-prompt-history-leak'\n\
         _cosh_test_accept_line() {\n\
           [[ \"$BUFFER\" == \"$COSH_TEST_PATH_PROMPT\" ]] && print -r -- path-prompt-user-widget-ran\n\
           zle .accept-line\n\
         }\n\
         zle -N accept-line _cosh_test_accept_line\n\
         _cosh_test_post_intercept_widget() { print -r -- path-prompt-post-intercept-widget-ran; zle .redisplay; }\n\
         zle -N _cosh_test_post_intercept_widget\n\
         bindkey '^Xb' _cosh_test_post_intercept_widget\n\
         _cosh_test_path_history() {\n\
           [[ \"$1\" == *\"$COSH_TEST_PATH_PROMPT\"* ]] && print -r -- path-prompt-history-hook-ran\n\
           return 0\n\
         }\n\
         autoload -Uz add-zsh-hook\n\
         add-zsh-hook zshaddhistory _cosh_test_path_history\n",
    )
    .unwrap();
    let home_str = home.to_string_lossy().to_string();
    let prompt = "/nonexistent-cosh-1943/SKILL.md 帮我读一下";
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (
                b"echo private-sequence-command-ran".to_vec(),
                Duration::from_millis(300),
            ),
            (b"\x1b[99;99u".to_vec(), Duration::from_millis(100)),
            (b"\n".to_vec(), Duration::from_millis(300)),
            (prompt.as_bytes().to_vec(), Duration::from_millis(300)),
            (b"\n".to_vec(), Duration::from_millis(50)),
            (b"\x18b".to_vec(), Duration::from_millis(1_000)),
            (
                b"fc -l -20 | grep -Fq -- \"$COSH_TEST_PATH_PROMPT\" && print \"$COSH_TEST_HISTORY_LEAK\"\n"
                    .to_vec(),
                Duration::from_millis(100),
            ),
            (
                b"echo after-zsh-path-prompt\n".to_vec(),
                Duration::from_millis(100),
            ),
            (b"TRAPURG\n".to_vec(), Duration::from_millis(100)),
            (b"exit 0\n".to_vec(), Duration::from_millis(100)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    let routed = format!("Received shell prompt request: {prompt}");
    let routed_offset = output.find(&routed).unwrap_or_else(|| panic!("{output}"));
    let history_probe_offset = output
        .find("fc -l -20")
        .unwrap_or_else(|| panic!("{output}"));
    let preserved_status_offset = output[routed_offset..]
        .find("zsh-path:0>")
        .map(|offset| routed_offset + offset)
        .unwrap_or_else(|| panic!("{output}"));
    assert!(preserved_status_offset < history_probe_offset, "{output}");
    assert!(output.contains("after-zsh-path-prompt"), "{output}");
    assert!(!output.contains("no such file or directory"), "{output}");
    assert!(!output.contains("path-prompt-user-trap-ran"), "{output}");
    assert!(output.contains("path-prompt-user-urg-trap-ran"), "{output}");
    assert!(!output.contains("path-prompt-user-widget-ran"), "{output}");
    assert!(output.contains("private-sequence-command-ran"), "{output}");
    assert!(
        output.contains("path-prompt-post-intercept-widget-ran"),
        "{output}"
    );
    assert_eq!(
        output.matches("path-prompt-private-binding-ran").count(),
        1,
        "{output}"
    );
    assert!(
        !output.contains("path-prompt-fork-channel-exposed"),
        "{output}"
    );
    assert!(!output.contains("path-prompt-history-hook-ran"), "{output}");
    assert!(!output.contains("path-prompt-history-leak"), "{output}");
}

#[test]
fn raw_cli_zsh_keeps_slash_named_function_and_alias_shell_owned() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("slash-named-shell-commands");
    // Exercise the complete snapshot rather than the line-init fallback.
    fs::write(
        home.join(".zshrc"),
        "zle -D zle-line-init 2>/dev/null || true\n",
    )
    .unwrap();
    let home_str = home.to_string_lossy().to_string();
    let function_input = "路径/run 帮我运行一下";
    let alias_input = "路径/alias 帮我运行一下";
    let global_alias_input = "路径/global 帮我运行一下";
    let suffix_alias_input = "/nonexistent-cosh-1943/suffix.txt 帮我运行一下";
    let long_name = format!("{0}/{0}/{0}/{0}/{0}", "路".repeat(80));
    let long_name_input = format!("{long_name} 帮我运行一下");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (
                b"function \xe8\xb7\xaf\xe5\xbe\x84/run { print -r -- function-path-command:$*; }\n"
                    .to_vec(),
                Duration::from_millis(300),
            ),
            (
                b"alias '\xe8\xb7\xaf\xe5\xbe\x84/alias'='print -r -- alias-path-command:'\n"
                    .to_vec(),
                Duration::from_millis(300),
            ),
            (
                b"alias -g '\xe8\xb7\xaf\xe5\xbe\x84/global=print -r -- global-path-command:'\n"
                    .to_vec(),
                Duration::from_millis(300),
            ),
            (
                b"alias -s txt='print -r -- suffix-path-command:'\n".to_vec(),
                Duration::from_millis(300),
            ),
            (
                format!("function {long_name} {{ print -r -- long-path-command:$*; }}\n").into_bytes(),
                Duration::from_millis(300),
            ),
            (
                format!("{function_input}\n").into_bytes(),
                Duration::from_millis(300),
            ),
            (
                format!("{alias_input}\n").into_bytes(),
                Duration::from_millis(300),
            ),
            (
                format!("{global_alias_input}\n").into_bytes(),
                Duration::from_millis(300),
            ),
            (
                format!("{suffix_alias_input}\n").into_bytes(),
                Duration::from_millis(300),
            ),
            (format!("{long_name_input}\n").into_bytes(), Duration::from_millis(300)),
            (b"exit 0\n".to_vec(), Duration::from_millis(300)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("function-path-command:帮我运行一下"),
        "{output}"
    );
    assert!(
        output.contains("alias-path-command: 帮我运行一下"),
        "{output}"
    );
    assert!(
        output.contains("global-path-command: 帮我运行一下"),
        "{output}"
    );
    assert!(
        output.contains("suffix-path-command: /nonexistent-cosh-1943/suffix.txt 帮我运行一下"),
        "{output}"
    );
    assert!(
        output.contains("long-path-command:帮我运行一下"),
        "{output}"
    );
    assert!(
        !output.contains(&format!("Received shell prompt request: {function_input}")),
        "{output}"
    );
    assert!(
        !output.contains(&format!("Received shell prompt request: {alias_input}")),
        "{output}"
    );
    for input in [
        global_alias_input,
        suffix_alias_input,
        long_name_input.as_str(),
    ] {
        assert!(
            !output.contains(&format!("Received shell prompt request: {input}")),
            "{output}"
        );
    }
}

#[test]
fn raw_cli_zsh_keeps_slash_function_after_line_init_mutation_shell_owned() {
    assert_slash_function_after_line_init_mutation_shell_owned("_cosh_test_line_init");
}

#[test]
fn raw_cli_zsh_keeps_slash_function_after_self_bound_line_init_mutation_shell_owned() {
    assert_slash_function_after_line_init_mutation_shell_owned("zle-line-init");
}

fn assert_slash_function_after_line_init_mutation_shell_owned(hook: &str) {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("slash-function-line-init");
    fs::write(
        home.join(".zshrc"),
        format!(
            "PROMPT='zsh-line-init:%?> '\nRPROMPT=''\nbindkey -e\n\
             {hook}() {{ function 路径/late {{ print -r -- line-init-path-command:$*; }}; }}\n\
             zle -N zle-line-init {hook}\n"
        ),
    )
    .unwrap();
    let home_str = home.to_string_lossy().to_string();
    let input = "路径/late 帮我运行一下";
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (
                format!("{input}\n").into_bytes(),
                Duration::from_millis(300),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(300)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("line-init-path-command:帮我运行一下"),
        "{output}"
    );
    assert!(
        !output.contains(&format!("Received shell prompt request: {input}")),
        "{output}"
    );
}

#[test]
fn raw_cli_zsh_hands_buffered_tab_to_zle_before_follow_up_input() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("path-prompt-tab-handoff");
    fs::write(
        home.join(".zshrc"),
        "PROMPT='zsh-tab:%?> '\nRPROMPT=''\nbindkey -e\n\
         _cosh_test_tab_rewrite() {\n\
           print -r -- path-prompt-tab-widget-ran\n\
           BUFFER=': > \"$HOME/path-prompt-tab-command-executed\"'\n\
           CURSOR=${#BUFFER}\n\
           zle .redisplay\n\
         }\n\
         zle -N _cosh_test_tab_rewrite\n\
         bindkey '^I' _cosh_test_tab_rewrite\n",
    )
    .unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        vec![
            (
                b"/definitely-not-command".to_vec(),
                Duration::from_millis(300),
            ),
            (
                b"\t\r echo path-prompt-typeahead-preserved\r exit 0\r".to_vec(),
                Duration::from_millis(300),
            ),
            // Hold stdin open without producing another read; the handoff
            // deadline must drain the complete retained tail on its own.
            (Vec::new(), Duration::from_millis(300)),
        ],
    );
    assert!(output.contains("path-prompt-tab-widget-ran"), "{output}");
    assert!(!home.join("path-prompt-tab-command-executed").exists());
    assert!(
        output.contains("path-prompt-typeahead-preserved"),
        "{output}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn raw_cli_shell_only_keeps_slash_bearing_han_prompt_shell_owned() {
    let prompt = "打开./nonexistent-cosh-2913";
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (b"\x1b[Z".to_vec(), Duration::from_millis(300)),
            (prompt.as_bytes().to_vec(), Duration::from_millis(100)),
            (b"\n".to_vec(), Duration::from_millis(50)),
            (b"exit 0\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert!(
        !output.contains("Received shell prompt request"),
        "{output}"
    );
    assert!(output.contains("No such file or directory"), "{output}");
}

#[test]
fn raw_cli_routes_path_prompt_after_shell_cwd_change() {
    let prompt = "打开./nonexistent-cosh-2913/SKILL.md";
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (b"cd /tmp\n".to_vec(), Duration::from_millis(300)),
            (prompt.as_bytes().to_vec(), Duration::from_millis(200)),
            (b"\n".to_vec(), Duration::from_millis(50)),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(
        output.contains(&format!("Received shell prompt request: {prompt}")),
        "{output}"
    );
    assert!(!output.contains("No such file or directory"), "{output}");
}

#[test]
fn raw_cli_uses_physical_cwd_when_pwd_is_reassigned() {
    let work_dir = tempfile::tempdir().expect("temp work dir");
    let executable_dir = work_dir.path().join("打开.");
    fs::create_dir(&executable_dir).expect("create executable dir");
    let executable = executable_dir.join("existing");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf 'physical-cwd-shell-owned\\n'\n",
    )
    .expect("write executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("chmod executable");

    for (shell, args) in [("bash", vec![]), ("zsh", vec!["--shell", "zsh"])] {
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
            "fake",
            &args,
            &[
                ("COSH_SHELL_INTEGRATION", "enhanced"),
                ("COSH_SHELL_STARTUP_BANNER", "0"),
                ("LANG", "C.UTF-8"),
                ("LC_ALL", "C.UTF-8"),
            ],
            work_dir.path(),
            vec![
                (b"PWD=/tmp\n".to_vec(), Duration::from_millis(300)),
                (
                    "打开./existing\n".as_bytes().to_vec(),
                    Duration::from_millis(300),
                ),
                (b"exit 0\n".to_vec(), Duration::from_millis(300)),
            ],
        );

        assert!(
            output.contains("physical-cwd-shell-owned"),
            "{shell}: {output}"
        );
        assert!(
            !output.contains("Received shell prompt request"),
            "{shell}: {output}"
        );
    }
}

#[test]
fn raw_cli_rejects_physical_cwd_with_trailing_newline() {
    let work_dir = tempfile::tempdir().expect("temp work dir");
    let plain_dir = work_dir.path().join("cwd");
    let newline_dir = work_dir.path().join("cwd\n");
    fs::create_dir(&plain_dir).expect("create plain cwd");
    fs::create_dir(&newline_dir).expect("create newline cwd");

    let executable_dir = newline_dir.join("打开.");
    fs::create_dir(&executable_dir).expect("create executable dir");
    let executable = executable_dir.join("existing");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf 'newline-cwd-shell-owned\\n'\n",
    )
    .expect("write executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("chmod executable");

    for (shell, args) in [("bash", vec![]), ("zsh", vec!["--shell", "zsh"])] {
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
            "fake",
            &args,
            &[
                ("COSH_SHELL_INTEGRATION", "enhanced"),
                ("COSH_SHELL_STARTUP_BANNER", "0"),
                ("LANG", "C.UTF-8"),
                ("LC_ALL", "C.UTF-8"),
            ],
            &newline_dir,
            vec![
                (
                    "打开./existing\n".as_bytes().to_vec(),
                    Duration::from_millis(300),
                ),
                (b"exit 0\n".to_vec(), Duration::from_millis(300)),
            ],
        );

        assert!(
            output.contains("newline-cwd-shell-owned"),
            "{shell}: {output}"
        );
        assert!(
            !output.contains("Received shell prompt request"),
            "{shell}: {output}"
        );
    }
}

#[test]
fn raw_cli_keeps_batched_path_prompt_shell_owned() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (
                b"echo batch-before-path\n\xe6\x89\x93\xe5\xbc\x80./nonexistent-cosh-2913 API Key: sk-review-fixture\n"
                    .to_vec(),
                Duration::from_millis(300),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(output.contains("batch-before-path"), "{output}");
    assert!(output.contains("No such file or directory"), "{output}");
    assert!(
        !output.contains("Received shell prompt request"),
        "{output}"
    );
}

#[test]
fn raw_cli_keeps_slash_candidate_batch_shell_owned() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (b"/".to_vec(), Duration::from_millis(100)),
            (
                b"bin/true\n\xe6\x89\x93\xe5\xbc\x80./nonexistent-cosh-2913\n".to_vec(),
                Duration::from_millis(300),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(output.contains("No such file or directory"), "{output}");
    assert!(
        !output.contains("Received shell prompt request"),
        "{output}"
    );
}

#[test]
fn raw_cli_zsh_shell_arg_intercepts_fragmented_agent_marker() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"?? zsh ".to_vec(), Duration::ZERO),
            (b"fragmented agent\n".to_vec(), Duration::from_millis(50)),
            (
                b"echo after-zsh-agent\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert_agent_loading_visible(&output);
    assert!(
        output.contains("Received shell prompt request: ?? zsh fragmented agent"),
        "{output}"
    );
    assert!(output.contains("after-zsh-agent"), "{output}");
    assert!(!output.contains("zsh: command not found: ??"), "{output}");
    assert!(!output.contains("\x1b]1337;COSH;"), "{output}");
}

#[test]
fn raw_cli_zsh_shell_arg_intercepts_fragmented_natural_language() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        vec![
            (b"\xe4\xbd".to_vec(), Duration::ZERO),
            (b"\xa0\xe5\xa5\xbd\n".to_vec(), Duration::from_millis(50)),
            (
                b"echo after-zsh-natural\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("\u{4f60}\u{597d}"), "{output}");
    assert_agent_loading_visible(&output);
    assert!(
        output.contains("Received shell prompt request: \u{4f60}\u{597d}"),
        "{output}"
    );
    assert!(output.contains("after-zsh-natural"), "{output}");
    assert!(
        !output.contains("zsh: command not found: \u{4f60}\u{597d}"),
        "{output}"
    );
}

#[test]
fn raw_cli_natural_language_omits_recent_command_facts_by_default() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"echo shell-context-ok\n".to_vec(), Duration::ZERO),
            (
                b"please show context\n".to_vec(),
                Duration::from_millis(100),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );

    assert!(output.contains("shell-context-ok"), "{output}");
    assert!(
        output.contains("Recent context visible to Agent: <none>"),
        "{output}"
    );
    let no_wrap: String = output.replace('│', "");
    assert!(
        !no_wrap.contains("command=echo shell-context-ok"),
        "{output}"
    );
    assert!(
        !no_wrap.contains("output_id=terminal-output://raw-session-"),
        "{output}"
    );
    assert!(!no_wrap.contains("/cmd-1"), "{output}");
    assert!(
        !no_wrap.contains("output_id=terminal-output://raw-session/cmd-1"),
        "{output}"
    );
    assert!(!no_wrap.contains("command=exit"), "{output}");
    assert!(!no_wrap.contains("preview:"), "{output}");
    assert!(!output.contains("ref="), "{output}");
    assert!(!output.contains("/output-refs/"), "{output}");
}

#[test]
fn raw_cli_delays_agent_output_while_foreground_command_is_active() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? hold test slow agent\n".to_vec(), Duration::ZERO),
            (
                b"sleep 0.3; echo after-foreground\n".to_vec(),
                Duration::from_millis(200),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(3_500)),
        ],
    );

    assert!(output.contains("Thinking..."), "{output}");
    assert!(output.contains("after-foreground"), "{output}");
    assert!(
        output.contains("Slow fake response for: ?? hold test slow agent"),
        "{output}"
    );
    assert_inline_before_followup(
        &output,
        "after-foreground",
        "Slow fake response for: ?? hold test slow agent",
    );
}

#[test]
fn raw_cli_agent_marker_invokes_adapter_without_failed_command() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"?? check current directory\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );

    assert!(output.contains("Thinking..."));
    assert!(output.contains("Received shell prompt request: ?? check current directory"));
    assert!(!output.contains("command exited with code"));
    assert_no_prompt_between(&output, "Thinking...", "Received shell prompt request");
}

#[test]
fn raw_cli_zh_natural_language_intercept_skips_redundant_notice() {
    if !bash_supports_command_not_found_handler() {
        return;
    }
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            ("帮我看看当前目录\n".as_bytes().to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );

    assert!(!output.contains("AI 请求"), "{output}");
    assert!(!output.contains("Agent 输入"), "{output}");
    assert!(
        !output.contains("该输入已在 Shell 执行前被拦截。"),
        "{output}"
    );
    assert!(output.contains("正在思考..."), "{output}");
    assert!(
        output.contains("已收到 Shell 提示请求：帮我看看当前目录"),
        "{output}"
    );
    assert!(
        !output.contains("Received shell prompt request"),
        "{output}"
    );
    assert!(!output.contains("bash: 帮我看看当前目录"), "{output}");
}

#[test]
fn raw_cli_zsh_agent_response_restores_prompt_without_empty_command() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("agent-prompt");
    fs::write(home.join(".zshrc"), "PROMPT='ZPROMPT> '\nRPROMPT=''\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            (b"?? zsh prompt smoke\n".to_vec(), Duration::ZERO),
            (
                b"echo after-agent\nexit\n".to_vec(),
                Duration::from_millis(1200),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("Received shell prompt request: ?? zsh prompt smoke"),
        "{output}"
    );
    assert!(output.contains("after-agent"), "{output}");
    assert!(count_occurrences(&output, "ZPROMPT> ") >= 2, "{output}");
    assert!(
        count_occurrences_between(
            &output,
            "Received shell prompt request: ?? zsh prompt smoke",
            "echo after-agent",
            "ZPROMPT> "
        ) >= 1,
        "{output}"
    );
    assert_no_standalone_percent_line(&output);
}

#[test]
fn raw_cli_bash_agent_prompt_restore_does_not_duplicate_prompt() {
    let home = temp_shell_home("agent-prompt-bash");
    fs::write(home.join(".bashrc"), "PS1='BPROMPT> '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            (b"?? bash prompt smoke\n".to_vec(), Duration::ZERO),
            (
                b"echo after-agent\nexit\n".to_vec(),
                Duration::from_millis(1200),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("Received shell prompt request: ?? bash prompt smoke"),
        "{output}"
    );
    assert!(output.contains("after-agent"), "{output}");
    let prompt_count = count_occurrences_between(
        &output,
        "Received shell prompt request: ?? bash prompt smoke",
        "echo after-agent",
        "BPROMPT> ",
    );
    assert!(
        (1..=2).contains(&prompt_count),
        "prompt_count={prompt_count}\n{output}"
    );
}

#[test]
fn raw_cli_zsh_agent_prompt_restore_suppresses_partial_line_marker() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("agent-prompt-sp");
    fs::write(
        home.join(".zshrc"),
        "PROMPT='ZPROMPT> '\n\
         RPROMPT=''\n\
         autoload -Uz add-zsh-hook\n\
         _cosh_test_force_prompt_sp() { setopt PROMPT_SP PROMPT_CR; }\n\
         add-zsh-hook precmd _cosh_test_force_prompt_sp\n",
    )
    .unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            (b"?? zsh prompt sp smoke\n".to_vec(), Duration::ZERO),
            (
                b"echo after-agent\nexit\n".to_vec(),
                Duration::from_millis(1200),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("Received shell prompt request: ?? zsh prompt sp smoke"),
        "{output}"
    );
    assert!(output.contains("after-agent"), "{output}");
    assert!(count_occurrences(&output, "ZPROMPT> ") >= 2, "{output}");
    let response = output
        .find("Received shell prompt request: ?? zsh prompt sp smoke")
        .expect("agent response");
    let next_command = response
        + output[response..]
            .find("echo after-agent")
            .expect("next shell command");
    let restored = &output[response..next_command];
    assert!(restored.contains("ZPROMPT> "), "{output}");
    // This contract covers Cosh's Agent-to-Shell restoration. The user's
    // precmd re-enables PROMPT_SP for subsequent ordinary Shell commands.
    assert_no_standalone_percent_line(restored);
}

#[test]
fn raw_cli_zsh_shell_marker_agent_response_does_not_duplicate_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_shell_home("agent-shell-marker-zsh");
    fs::write(home.join(".zshrc"), "PROMPT='ZPROMPT> '\nRPROMPT=''\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            ("\u{4f60}\u{597d}\n".as_bytes().to_vec(), Duration::ZERO),
            (
                b"echo after-agent\nexit\n".to_vec(),
                Duration::from_millis(1200),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("Received shell prompt request: \u{4f60}\u{597d}"),
        "{output}"
    );
    assert!(output.contains("after-agent"), "{output}");
    assert_eq!(count_occurrences(&output, "ZPROMPT> "), 3, "{output}");
    assert_eq!(
        count_occurrences_between(
            &output,
            "Received shell prompt request: \u{4f60}\u{597d}",
            "echo after-agent",
            "ZPROMPT> "
        ),
        1,
        "{output}"
    );
    assert_no_standalone_percent_line(&output);
}

#[test]
fn raw_cli_bash_shell_marker_agent_response_does_not_duplicate_prompt() {
    if !bash_supports_command_not_found_handler() {
        return;
    }
    let home = temp_shell_home("agent-shell-marker-bash");
    fs::write(home.join(".bashrc"), "PS1='BPROMPT> '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            ("\u{4f60}\u{597d}\n".as_bytes().to_vec(), Duration::ZERO),
            (
                b"echo after-agent\nexit\n".to_vec(),
                Duration::from_millis(1200),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("Received shell prompt request: \u{4f60}\u{597d}"),
        "{output}"
    );
    assert!(output.contains("after-agent"), "{output}");
    assert_eq!(count_occurrences(&output, "BPROMPT> "), 3, "{output}");
    assert_eq!(
        count_occurrences_between(
            &output,
            "Received shell prompt request: \u{4f60}\u{597d}",
            "echo after-agent",
            "BPROMPT> "
        ),
        1,
        "{output}"
    );
}

#[test]
fn raw_cli_empty_enter_and_ctrl_c_do_not_start_agent() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(50)),
            (b"\nexit 0\n".to_vec(), Duration::from_millis(50)),
        ],
    );

    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("Command failed:"), "{output}");
    assert!(!output.contains("Agent status"), "{output}");
    assert!(output.contains("exit 0"), "{output}");
}

#[test]
fn raw_cli_empty_enter_after_agent_response_does_not_retrigger() {
    if !bash_supports_command_not_found_handler() {
        return;
    }
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            ("\u{4f60}\u{597d}\n".as_bytes().to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(200)),
            (b"exit\n".to_vec(), Duration::from_millis(50)),
        ],
    );

    assert_eq!(agent_loading_count(&output), 1, "{output}");
    assert_eq!(
        count_occurrences(&output, "Received shell prompt request"),
        1,
        "{output}"
    );
    let response_pos = output
        .find("Received shell prompt request")
        .expect("agent response");
    let prompt_after_response = output[response_pos..]
        .find("cosh-osc$")
        .expect("prompt after agent response");
    assert!(prompt_after_response > 0, "{output}");
}

#[test]
fn raw_cli_non_ascii_agent_input_echoes_before_intercept() {
    if !bash_supports_command_not_found_handler() {
        return;
    }
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            ("\u{4f60}".as_bytes().to_vec(), Duration::ZERO),
            ("\u{597d}".as_bytes().to_vec(), Duration::from_millis(50)),
            (b"\n".to_vec(), Duration::from_millis(50)),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );
    let normalized = strip_ansi_escape(&output);

    assert!(
        normalized.contains("cosh-osc$ \u{4f60}\u{597d}"),
        "{output}"
    );
    assert_eq!(
        count_occurrences(&output, "\n\u{4f60}\u{597d}"),
        0,
        "{output}"
    );
    assert!(
        output.contains("Received shell prompt request: \u{4f60}\u{597d}"),
        "{output}"
    );
    assert!(output.contains("cosh-osc$ exit"), "{output}");
    assert!(!output.contains("bash: \u{4f60}\u{597d}"), "{output}");
}

#[test]
fn raw_cli_non_ascii_shell_input_supports_backspace() {
    if !bash_supports_command_not_found_handler() {
        return;
    }
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            ("\u{4f60}".as_bytes().to_vec(), Duration::ZERO),
            ("\u{597d}".as_bytes().to_vec(), Duration::from_millis(50)),
            (vec![0x7f], Duration::from_millis(50)),
            ("\u{5417}\n".as_bytes().to_vec(), Duration::from_millis(50)),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );
    let normalized = strip_ansi_escape(&output);
    let response_pos = normalized
        .find("Received shell prompt request")
        .expect("agent response");
    let echo = &normalized[..response_pos];

    assert!(echo.contains("cosh-osc$"), "{output}");
    assert!(
        echo.contains('\u{4f60}') && echo.contains('\u{5417}'),
        "{output}"
    );
    assert!(
        output.contains("Received shell prompt request: \u{4f60}\u{5417}"),
        "{output}"
    );
    assert!(
        !output.contains("Received shell prompt request: \u{4f60}\u{597d}\u{5417}"),
        "{output}"
    );
    assert!(!output.contains("bash: \u{4f60}\u{5417}"), "{output}");
}

#[test]
fn routing_c3_explicit_draft_soft_newline_composes_multiline_prompt() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                "?? \u{8bf7}\u{5e2e}\u{6211}\u{5206}\u{6790}"
                    .as_bytes()
                    .to_vec(),
                Duration::ZERO,
            ),
            (b"\x1b[13;2u".to_vec(), Duration::from_millis(50)),
            (
                "\u{7ed9}\u{51fa}\u{5efa}\u{8bae}\n".as_bytes().to_vec(),
                Duration::from_millis(50),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    // Panel rendering flattens the newline for display, so assert one single
    // aggregated request carrying both segments instead of the raw LF.
    assert_eq!(
        output.matches("Received shell prompt request:").count(),
        1,
        "draft must submit exactly one aggregated request: {output}"
    );
    assert!(
        output.contains("\u{8bf7}\u{5e2e}\u{6211}\u{5206}\u{6790}")
            && output.contains("\u{7ed9}\u{51fa}\u{5efa}\u{8bae}"),
        "both draft segments must reach the agent: {output}"
    );
    assert!(!output.contains(";2u"), "no CSI-u leak: {output}");
    assert!(
        !output.contains("bash: \u{8bf7}\u{5e2e}\u{6211}\u{5206}\u{6790}"),
        "draft must not flush to bash: {output}"
    );
}

#[test]
fn routing_c3_explicit_draft_bracketed_paste_newlines_do_not_submit_early() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? ".to_vec(), Duration::ZERO),
            (
                {
                    let mut paste = b"\x1b[200~".to_vec();
                    paste.extend_from_slice(
                        "\u{5206}\u{6790}\u{8d1f}\u{8f7d}\r\n\u{7ed9}\u{51fa}\u{5efa}\u{8bae}"
                            .as_bytes(),
                    );
                    paste.extend_from_slice(b"\x1b[201~");
                    paste
                },
                Duration::ZERO,
            ),
            (b"\n".to_vec(), Duration::from_millis(100)),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert_eq!(
        output.matches("Received shell prompt request:").count(),
        1,
        "paste must submit exactly one aggregated request: {output}"
    );
    assert!(
        output.contains("\u{5206}\u{6790}\u{8d1f}\u{8f7d}")
            && output.contains("\u{7ed9}\u{51fa}\u{5efa}\u{8bae}"),
        "both pasted lines must reach the agent together: {output}"
    );
}

#[test]
fn routing_c3_wrapped_paste_split_opener_stays_shell_owned() {
    if !bash_supports_command_not_found_handler() {
        return;
    }
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"\x1b[2".to_vec(), Duration::ZERO),
            (
                {
                    let mut tail = b"00~".to_vec();
                    tail.extend_from_slice(b"printf SPLIT_PASTE_OK");
                    tail.extend_from_slice(b"\x1b[201~");
                    tail
                },
                Duration::from_millis(10),
            ),
            (b"\n".to_vec(), Duration::from_millis(100)),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert!(
        output.contains("SPLIT_PASTE_OK"),
        "split opener paste must execute through the shell: {output}"
    );
    assert!(
        !output.contains("Prompt draft"),
        "ordinary paste must not open an Agent draft: {output}"
    );
}

#[test]
fn routing_c3_explicit_draft_shows_composition_hint() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                "?? \u{8bf7}\u{5e2e}\u{6211}\u{5206}\u{6790}"
                    .as_bytes()
                    .to_vec(),
                Duration::ZERO,
            ),
            (b"\x1b[13;2u".to_vec(), Duration::from_millis(50)),
            (b"\x1b".to_vec(), Duration::from_millis(400)),
            (b"exit\n".to_vec(), Duration::from_millis(400)),
        ],
    );

    assert!(
        output.contains("Prompt draft"),
        "the draft card must open on soft newline: {output}"
    );
    assert!(
        output.contains("Enter send \u{b7} Shift+Enter newline \u{b7} Esc cancel"),
        "card footer must carry the key guidance: {output}"
    );
    assert!(
        output.contains("Draft cancelled"),
        "Esc must freeze the card as cancelled: {output}"
    );
}

#[test]
fn raw_cli_passthrough_shortcut_shows_one_time_tip() {
    // #1721 matrix #18 (T-c): after cursor movement makes the Readline buffer
    // impossible to mirror safely, the shortcut is stripped and surfaces a
    // one-time discoverability tip at the next prompt.
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"echo tip-probe".to_vec(), Duration::ZERO),
            (b"\x1b[D".to_vec(), Duration::from_millis(50)),
            (b"\x1b[13;2u".to_vec(), Duration::from_millis(50)),
            (b"\n".to_vec(), Duration::from_millis(100)),
            (b"echo tip-once\n".to_vec(), Duration::from_millis(400)),
            (b"exit\n".to_vec(), Duration::from_millis(400)),
        ],
    );

    assert!(
        output.contains("Tip: start with ?? to compose multi-line prompts"),
        "one-time tip must appear after prompt-ready: {output}"
    );
    assert_eq!(
        output
            .matches("Tip: start with ?? to compose multi-line prompts")
            .count(),
        1,
        "tip must render exactly once per session: {output}"
    );
    assert!(output.contains("tip-probe"), "{output}");
}
