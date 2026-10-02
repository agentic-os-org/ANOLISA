use super::*;

#[test]
fn marker_json_escape_removes_raw_controls_and_preserves_utf8() {
    let input = concat!(
        "A中\"\\",
        "\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}\u{8}\t\n\u{b}\u{c}\r",
        "\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{14}\u{15}\u{16}\u{17}",
        "\u{18}\u{19}\u{1a}\u{1b}\\\u{1c}\u{1d}\u{1e}\u{1f}\u{7f}Z"
    );
    let expected = concat!(
        "A中\\\"\\\\",
        "\\u0001\\u0002\\u0003\\u0004\\u0005\\u0006\\u0007\\b\\t\\n\\u000b\\f\\r",
        "\\u000e\\u000f\\u0010\\u0011\\u0012\\u0013\\u0014\\u0015\\u0016\\u0017",
        "\\u0018\\u0019\\u001a\\u001b\\\\\\u001c\\u001d\\u001e\\u001f\\u007fZ"
    );
    let scripts = [
        ("bash", include_str!("../../src/shell_host/marker/bash.sh")),
        ("zsh", include_str!("../../src/shell_host/marker/zsh.rs")),
    ];

    for (shell, script) in scripts {
        if Command::new(shell).arg("--version").output().is_err() {
            continue;
        }
        let function = shell_function(script, "_cosh_json_escape");
        let command = format!("{function}\n_cosh_json_escape \"$1\"");
        let output = Command::new(shell)
            .args(["-c", command.as_str(), "cosh-marker-test", input])
            .output()
            .unwrap_or_else(|error| panic!("run {shell}: {error}"));

        assert!(output.status.success(), "{shell}: {output:?}");
        assert_eq!(output.stdout, expected.as_bytes(), "{shell}");
        assert!(
            output.stdout.iter().all(|byte| !byte.is_ascii_control()),
            "{shell}: {:?}",
            output.stdout
        );
    }
}

fn shell_function<'a>(script: &'a str, name: &str) -> &'a str {
    let start_pattern = format!("{name}() {{\n");
    let start = script
        .find(&start_pattern)
        .unwrap_or_else(|| panic!("missing {name}"));
    let rest = &script[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("unterminated {name}"));
    &rest[..end + 2]
}

#[test]
fn shell_secret_detectors_match_canonical_shapes_and_boundaries() {
    let detectors = [
        ("bash", include_str!("../../src/shell_host/marker/bash.sh")),
        (
            "zsh",
            include_str!("../../src/shell_host/marker/zsh_secret_detector.zsh"),
        ),
    ];
    let sensitive = [
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature",
        "inspect eyJabcde.fghijk.lmnopq now",
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "Bearer abc",
        "https://user:password@example.test/path",
        "http://:s3cr3t42@example.test/",
        "ghp_abcdefghijklmnopqrstuvwxyz123456",
        "github_pat_abcdefghijklmnopqrstuvwxyz123456",
        "glpat-abcdefghijklmnop",
        "npm_abcdefghijklmnopqrstuvwxyz123456",
        "hf_abcdefghijklmnopqrstuvwxyz123456",
        "xoxb-1234567890abcdefghijklmnop",
        "AIzaabcdefghijklmnopqrstuvwxyz123456",
        "LTAI5tExampleAccessKey",
        "AKIA1234567890ABCDEF",
        "ASIA1234567890ABCDEF",
        "sk-fbaa6",
        "sk-abc123",
        "sk_live_abcdefghijklmnop",
        "sk_test_abcdefghijklmnop",
        "key是sk-abc123",
        "（sk-abc123）",
        "密钥为sk-abc123",
        "key：sk-abc123",
        "你好 \"token\" = TEST_ONLY_RECOVERY_SECRET",
        "inspect 'client-secret' : TEST_ONLY_RECOVERY_SECRET",
        "你好 AWS_SECRET_ACCESS_KEY=TEST_ONLY_RECOVERY_SECRET",
        "inspect 'aws-secret_access-key' : TEST_ONLY_RECOVERY_SECRET",
        "你好 AWS_ACCESS_KEY_ID = TEST_ONLY_RECOVERY_SECRET",
        "你好 OPENAI_API_KEY = TEST_ONLY_RECOVERY_SECRET",
        "你好 DASHSCOPE_API_KEY = TEST_ONLY_RECOVERY_SECRET",
        "你好 GITHUB_TOKEN = TEST_ONLY_RECOVERY_SECRET",
        "你好 ALIBABA_CLOUD_ACCESS_KEY_ID = TEST_ONLY_RECOVERY_SECRET",
        "MEMORY_OPENAI_API_KEY=TEST_ONLY_RECOVERY_SECRET",
        "MY_TOKEN=TEST_ONLY_RECOVERY_SECRET",
        "FOO_PASSWORD=TEST_ONLY_RECOVERY_SECRET",
        "AUTH_REFRESH_TOKEN=TEST_ONLY_RECOVERY_SECRET",
        "AUTHTOKEN=TEST_ONLY_RECOVERY_SECRET",
        "myAccessToken=TEST_ONLY_RECOVERY_SECRET",
        "dbPassword=TEST_ONLY_RECOVERY_SECRET",
        "serviceSecret=TEST_ONLY_RECOVERY_SECRET",
        "customApiKey=TEST_ONLY_RECOVERY_SECRET",
        "你好 MY_OPENAI_API_KEY = public-name",
        "tool --password 'correct horse battery staple'",
        "tool --access-token=TEST_ONLY_RECOVERY_SECRET",
        "curl --cookie session=supersecret",
        "tool --set-cookie csrf=supersecret",
        "curl --cookie=session=supersecret",
        "TOKEN=$SECRET",
        "API_KEY=${VALUE}",
        "tool --password \"$PASSWORD\"",
        "tool --token '$TOKEN'",
        r"token=\escaped",
        "TOKEN=`secret-command`",
        "TOKEN=秘密123",
        "cookie=session-secret",
        "set_cookie=session-secret",
        r#"{"cookie":"json-cookie-secret"}"#,
        "Cookie: session=session-secret",
        "Set-Cookie: session=session-secret; Path=/",
        r"curl -H Cookie:\ session=quoted-cookie-value-42",
        r"curl -H Cookie\:session=quoted-cookie-value-42",
        "curl -H Cookie':'session=quoted-cookie-value-42",
        "curl -H Coo'kie:'session=quoted-cookie-value-42",
        r"curl -H Set\-Cookie\:csrf=quoted-cookie-value-42",
        r#"curl -H"Cookie: session=quoted-cookie-value-42""#,
        "curl -H'Cookie: session=quoted-cookie-value-42'",
        "curl -HCookie:session=quoted-cookie-value-42",
        r#"curl --header="Set-Cookie: csrf=quoted-cookie-value-42""#,
        r"curl -HCookie\:session=quoted-cookie-value-42",
        "curl -sH'Cookie: session=quoted-cookie-value-42'",
        r#"curl -fsSLH"Cookie: session=quoted-cookie-value-42""#,
        r"curl -sHCookie\:session=quoted-cookie-value-42",
        "curl -s'H'Cookie:session=quoted-cookie-value-42",
        r"curl -s\HCookie:session=quoted-cookie-value-42",
        r#"curl "-HCookie: session=whole-option-secret-42""#,
        "curl '-HCookie: session=whole-option-secret-42'",
        r#"curl ${HEADER_OPTION}"Cookie: session=dynamic-option-secret-42" https://example.test"#,
        r#"curl ${HEADER_OPTION}'Cookie: session=dynamic-option-secret-42' https://example.test"#,
        r#"curl ${HEADER_OPTION#*;}"Cookie: session=parameter-pattern-secret-42" https://example.test"#,
        r#"curl ${HEADER_OPTION#*|}"Cookie: session=parameter-pipe-secret-42" https://example.test"#,
        r#"curl $(printf -- -H)"Cookie: session=dynamic-option-secret-42" https://example.test"#,
        r#"curl `printf -- -H`"Cookie: session=dynamic-option-secret-42" https://example.test"#,
        r#"curl $(true; printf -- -H)"Cookie: session=semicolon-dynamic-secret-42" https://example.test"#,
        r#"curl $(true && printf -- -H)"Cookie: session=and-dynamic-secret-42" https://example.test"#,
        r#"curl $(printf -- -H | cat)"Cookie: session=pipe-dynamic-secret-42" https://example.test"#,
        r#"curl `true; printf -- -H`"Cookie: session=backtick-dynamic-secret-42" https://example.test"#,
        r#"curl $(printf %s "$(printf -- -H)")"Cookie: session=nested-dynamic-secret-42" https://example.test"#,
    ];
    let safe = [
        "sk-hynix 内存条什么价格",
        "npm_package_version 怎么读",
        "cookie: 头是干什么的",
        "cookie: header meaning",
        "bearer 是什么认证方式",
        "shf_test 文件在哪",
        "xsk-abc123",
        "ghp_documentation",
        "npm_package_identifier",
        "hf_model_name",
        "https://example.test/path:docs@example",
        "http://:@example.test/",
        "eyJabcd.fghijk.lmnopq",
        "eyJabcde.fghi.lmnopq",
        "xeyJabcde.fghijk.lmnopq",
        "你好 token candidate",
        "inspect monkey = value",
        "inspect token =",
        "你好 AWS_ACCESS_KEY_ID candidate",
        "你好 AWS_ACCESS_KEY_ID =",
        "你好 MY_OPENAI_API_KEY_LABEL = public-name",
        "你好 MY_TOKEN_COUNT = 5",
        r"curl -H Cookie\:header-meaning",
        r"curl -H Crookie\:session=quoted-cookie-value-42",
        r#"tool x-H"Cookie: session=quoted-cookie-value-42""#,
        r#"tool x-sH"Cookie: session=quoted-cookie-value-42""#,
        r#"curl --sH"Cookie: session=quoted-cookie-value-42""#,
        r#"tool -s'Hx'Cookie:session=quoted-cookie-value-42"#,
        r#"curl --header-label="Cookie: session=quoted-cookie-value-42""#,
        r#"curl ${HEADER_OPTION}"Crookie: session=dynamic-option-secret-42""#,
        r#"curl ${HEADER_OPTION#*;}"Crookie: session=parameter-pattern-secret-42""#,
        r#"echo $HOME; tool xCookie:session=dynamic-option-secret-42"#,
        r#"echo $(printf x); tool xCookie:session=dynamic-option-secret-42"#,
    ];

    for (shell, script) in detectors {
        if Command::new(shell).arg("--version").output().is_err() {
            continue;
        }
        let function = shell_function(script, "_cosh_command_has_secret");
        let command = format!("{function}\n_cosh_command_has_secret \"$1\"");
        for input in sensitive {
            let status = Command::new(shell)
                .args(["-c", command.as_str(), "cosh-marker-test", input])
                .status()
                .unwrap_or_else(|error| panic!("run {shell} secret detector: {error}"));
            assert!(status.success(), "{shell}: secret not detected: {input}");
        }
        for input in safe {
            let status = Command::new(shell)
                .args(["-c", command.as_str(), "cosh-marker-test", input])
                .status()
                .unwrap_or_else(|error| panic!("run {shell} secret control: {error}"));
            assert!(!status.success(), "{shell}: safe control rejected: {input}");
        }
    }
}

#[test]
fn shell_host_marker_control_cwd_keeps_json_frame_intact() {
    let control_name = concat!(
        "cwd-中-",
        "\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}\u{8}\t\n\u{b}\u{c}\r",
        "\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{14}\u{15}\u{16}\u{17}",
        "\u{18}\u{19}\u{1a}\u{1b}\\\u{1c}\u{1d}\u{1e}\u{1f}\u{7f}"
    );
    let shell_control_name = concat!(
        "$'cwd-中-",
        "\\001\\002\\003\\004\\005\\006\\007\\010\\011\\012\\013\\014\\015",
        "\\016\\017\\020\\021\\022\\023\\024\\025\\026\\027",
        "\\030\\031\\032\\033\\\\\\034\\035\\036\\037\\177'"
    );
    let tail_sentinel = "__post_cwd_marker_tail__";

    for shell in ["bash", "zsh"] {
        if Command::new(shell).arg("--version").output().is_err() {
            continue;
        }
        let work_dir =
            std::env::temp_dir().join(format!("cosh-marker-control-{shell}-{}", unique_suffix()));
        let control_dir = work_dir.join(control_name);
        std::fs::create_dir_all(&control_dir).expect("control cwd");
        let path = format!(
            "/{tail_sentinel}:{}",
            std::env::var("PATH").unwrap_or_default()
        );
        let config = ShellHostConfig::new(format!("marker-control-{shell}"), &work_dir)
            .with_env("PATH", path);
        let cd = format!(
            "builtin cd -- {}/{shell_control_name}",
            shell_arg(&work_dir)
        );
        let command = "printf '__marker_frame_survived__\\n'";
        let output = if shell == "bash" {
            run_scripted_bash(
                &config,
                &[
                    ScriptedInput::user_line(cd.clone()),
                    ScriptedInput::user_line(command),
                ],
            )
        } else {
            run_scripted_zsh(
                &config,
                &[
                    ScriptedInput::user_line(cd.clone()),
                    ScriptedInput::user_line(command),
                ],
            )
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        let terminal = String::from_utf8_lossy(&output.terminal_output);

        assert!(
            terminal.contains("__marker_frame_survived__"),
            "{shell}: {terminal}"
        );
        assert!(!terminal.contains(tail_sentinel), "{shell}: {terminal}");
        assert!(
            output.events.iter().any(|event| {
                event.kind == ShellEventKind::CommandStarted
                    && event.command.as_deref() == Some(command)
                    && event.cwd.as_deref() == control_dir.to_str()
            }),
            "{shell}: {:?}",
            output.events
        );
        assert!(
            !output.events.iter().any(|event| {
                event.kind == ShellEventKind::ComponentFailed
                    && event.component.as_deref() == Some("osc_parser")
            }),
            "{shell}: {:?}",
            output.events
        );
        std::fs::remove_dir_all(&work_dir).expect("cleanup control cwd");
    }
}

#[test]
fn shell_host_runs_bash_pty_and_emits_command_events() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-host-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let tool_path = work_dir.join("tmp-tool");
    std::fs::write(&tool_path, "#!/bin/sh\necho path-ok\n").expect("tool script");
    make_executable(&tool_path);

    let config = ShellHostConfig::new("shell-host-test", &work_dir);
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("/explain last error"),
            ScriptedInput::user_line("please explain the last error"),
            ScriptedInput::user_line(tool_path.display().to_string()),
            ScriptedInput::user_line("echo ok"),
            ScriptedInput::user_line(r#"printf "a\n" | grep a"#),
            ScriptedInput::user_line("ls /path/that/does/not/exist"),
        ],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(
        output
            .events
            .iter()
            .any(|event| event.kind == ShellEventKind::ShellStarted),
        "{terminal}\n{:?}",
        output.events
    );
    assert!(
        output
            .events
            .iter()
            .any(|event| event.kind == ShellEventKind::ShellReady),
        "{terminal}\n{:?}",
        output.events
    );
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("/explain last error")
            && event.component.as_deref() == Some("slash")
    }));
    assert_eq!(
        output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("please explain the last error")
                && event.component.as_deref() == Some("natural_language")
        }),
        bash_supports_command_not_found_handler()
    );
    assert!(!output
        .terminal_output
        .windows(b"\x1b]1337;COSH;".len())
        .any(|window| window == b"\x1b]1337;COSH;"));

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    assert_eq!(replayed_events, output.events);

    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("tmp-tool") && block.exit_code == 0));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo ok") && block.exit_code == 0));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("grep a") && block.exit_code == 0));

    let failed = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("/path/that/does/not/exist"))
        .expect("failed command block");
    assert_ne!(failed.exit_code, 0);
    assert!(failed.shell_environment_generation.is_none());
    let output_ref = failed
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let output_ref_text = std::fs::read_to_string(output_ref).expect("output ref text");
    assert!(output_ref_text.contains("No such file") || output_ref_text.contains("cannot access"));
}

#[test]
fn shell_host_bash_valid_cue_named_function_wins_over_natural_language() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-valid-cue-function-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "Who() { printf '__who_function__:%s\\n' \"$*\"; }\n",
    )
    .expect("bashrc");
    let config = ShellHostConfig::new("valid-cue-function", &work_dir)
        .with_env("HOME", home_dir.display().to_string());

    let output = run_scripted_bash(&config, &[ScriptedInput::user_line("Who are you")])
        .expect("scripted bash");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(terminal.contains("__who_function__:are you"), "{terminal}");
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[test]
fn shell_host_bash_valid_cue_matrix_wins_over_natural_language() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-valid-cue-matrix-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    let bin_dir = work_dir.join("bin");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "alias Who='printf \"__alias_who__:%s\\\\n\"'\n",
    )
    .expect("bashrc");
    let kindly = bin_dir.join("Kindly");
    std::fs::write(
        &kindly,
        "#!/bin/sh\nprintf '__path_kindly__:%s\\n' \"$*\"\n",
    )
    .expect("Kindly executable");
    make_executable(&kindly);
    let han = bin_dir.join("帮我看看");
    std::fs::write(&han, "#!/bin/sh\nprintf '__han_path__:%s\\n' \"$*\"\n")
        .expect("Han executable");
    make_executable(&han);
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let config = with_raw_byte_readline(
        ShellHostConfig::new("valid-cue-matrix", &work_dir)
            .with_env("HOME", home_dir.display().to_string())
            .with_env("PATH", path),
    );

    let inputs = [
        "Who are you",
        "help file",
        "Kindly explain this",
        "帮我看看 当前目录",
    ];
    let output = run_scripted_bash(
        &config,
        &inputs
            .iter()
            .map(|input| ScriptedInput::user_line(*input))
            .collect::<Vec<_>>(),
    )
    .expect("scripted bash");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(terminal.contains("__alias_who__:are"), "{terminal}");
    assert!(
        terminal.contains("__path_kindly__:explain this"),
        "{terminal}"
    );
    assert!(terminal.contains("__han_path__:当前目录"), "{terminal}");
    for input in inputs {
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input)
                && event.component.as_deref() == Some("natural_language")
        }));
    }
}

#[test]
fn shell_host_zsh_valid_cue_matrix_wins_over_natural_language() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-valid-cue-matrix-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    let bin_dir = work_dir.join("bin");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    std::fs::write(
        home_dir.join(".zshrc"),
        "alias Who='printf \"__zsh_alias_who__:%s\\\\n\"'\n\
         how() { printf '__zsh_function_how__:%s\\n' \"$*\"; }\n",
    )
    .expect("zshrc");
    let kindly = bin_dir.join("Kindly");
    std::fs::write(
        &kindly,
        "#!/bin/sh\nprintf '__zsh_path_kindly__:%s\\n' \"$*\"\n",
    )
    .expect("Kindly executable");
    make_executable(&kindly);
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let config = ShellHostConfig::new("zsh-valid-cue-matrix", &work_dir)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("COSH_ZDOTDIR_ORIG", home_dir.display().to_string())
        .with_env("PATH", path);

    let inputs = [
        "Who are you",
        "how file",
        "Kindly explain this",
        "test this",
    ];
    let output = run_scripted_zsh(
        &config,
        &inputs
            .iter()
            .map(|input| ScriptedInput::user_line(*input))
            .collect::<Vec<_>>(),
    )
    .expect("scripted zsh");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(terminal.contains("__zsh_alias_who__:are"), "{terminal}");
    assert!(terminal.contains("__zsh_function_how__:file"), "{terminal}");
    assert!(
        terminal.contains("__zsh_path_kindly__:explain this"),
        "{terminal}"
    );
    for input in inputs {
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input)
                && event.component.as_deref() == Some("natural_language")
        }));
    }
}

#[test]
fn shell_host_bash_missing_natural_language_routes_without_command_block() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-missing-natural-language-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("missing-natural-language", &work_dir);
    config.native_mode = false;

    let output = run_scripted_bash(&config, &[ScriptedInput::user_line("Kindly explain this")])
        .expect("scripted bash");
    let intercept = output
        .events
        .iter()
        .find(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("Kindly explain this")
                && event.component.as_deref() == Some("natural_language")
        })
        .unwrap_or_else(|| panic!("natural-language intercept: {:?}", output.events));

    assert!(intercept.command_id.is_none(), "{:?}", output.events);
    assert!(intercept.routing.is_none(), "{:?}", output.events);
    let ledger = build_command_blocks(&output.events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert!(!ledger
        .blocks
        .iter()
        .any(|block| block.command == "Kindly explain this"));
    assert!(
        !String::from_utf8_lossy(&output.terminal_output).contains("command not found"),
        "{}",
        String::from_utf8_lossy(&output.terminal_output)
    );
}

#[test]
fn shell_host_zsh_missing_natural_language_closes_started_command() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-missing-natural-language-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-missing-natural-language", &work_dir);
    config.native_mode = false;

    for input in ["Kindly explain this", "Just do it"] {
        let output =
            run_scripted_zsh(&config, &[ScriptedInput::user_line(input)]).expect("scripted zsh");
        let intercept = output
            .events
            .iter()
            .find(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some(input)
                    && event.component.as_deref() == Some("natural_language")
            })
            .unwrap_or_else(|| panic!("natural-language intercept: {:?}", output.events));

        assert!(intercept.command_id.is_some(), "{:?}", output.events);
        assert!(
            intercept
                .routing
                .as_ref()
                .is_some_and(|routing| routing.top_level_missing && routing.proven),
            "{:?}",
            output.events
        );
        let ledger = build_command_blocks(&output.events);
        assert!(!ledger.blocks.iter().any(|block| block.command == input));
        assert!(
            !String::from_utf8_lossy(&output.terminal_output).contains("command not found"),
            "{}",
            String::from_utf8_lossy(&output.terminal_output)
        );
    }
}

#[test]
fn shell_host_han_parameter_expansion_routes_to_agent() {
    let inputs = ["帮我解释 $HOME 变量"];
    for shell in ["bash", "zsh"] {
        if Command::new(shell).arg("--version").output().is_err() {
            continue;
        }
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }

        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-han-metachar-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let mut config = ShellHostConfig::new(format!("han-metachar-{shell}"), &work_dir);
        config.native_mode = false;
        for input in inputs {
            let output = if shell == "bash" {
                run_scripted_bash(&config, &[ScriptedInput::user_line(input)])
            } else {
                run_scripted_zsh(&config, &[ScriptedInput::user_line(input)])
            }
            .unwrap_or_else(|error| panic!("{shell}: {input:?}: {error}"));
            assert!(
                output.events.iter().any(|event| {
                    event.kind == ShellEventKind::UserInputIntercepted
                        && event.input.as_deref() == Some(input)
                        && event.component.as_deref() == Some("natural_language")
                }),
                "{shell}: {input:?}: {:?}",
                output.events
            );
        }
    }
}

#[test]
fn shell_host_han_re_evaluated_parameter_expansion_stays_shell_owned() {
    for (shell, expansion) in [("bash", "${evil@P}"), ("zsh", "${(e)evil}")] {
        if Command::new(shell).arg("--version").output().is_err() {
            continue;
        }
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }

        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-han-re-eval-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let input = format!("解释 \"{expansion}\"");
        let mut config = ShellHostConfig::new(format!("han-re-eval-{shell}"), &work_dir);
        config.native_mode = false;
        let steps = [
            ScriptedInput::command("evil='$(printf __han_re_eval__)'"),
            ScriptedInput::user_line(&input),
        ];
        let output = if shell == "bash" {
            run_scripted_bash(&config, &steps)
        } else {
            run_scripted_zsh(&config, &steps)
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));

        assert!(
            !output.events.iter().any(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some(input.as_str())
            }),
            "{shell}: {:?}",
            output.events
        );
    }
}

#[test]
fn shell_host_han_control_operator_stays_shell_owned() {
    for shell in ["bash", "zsh"] {
        if Command::new(shell).arg("--version").output().is_err() {
            continue;
        }
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }

        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-han-control-operator-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let side_effect = work_dir.join("must-not-exist");
        let input = format!("解释 false && touch {}", side_effect.display());
        let mut config = ShellHostConfig::new(format!("han-control-operator-{shell}"), &work_dir);
        config.native_mode = false;
        let output = if shell == "bash" {
            run_scripted_bash(&config, &[ScriptedInput::user_line(&input)])
        } else {
            run_scripted_zsh(&config, &[ScriptedInput::user_line(&input)])
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));

        assert!(
            !output.events.iter().any(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some(input.as_str())
            }),
            "{shell}: {:?}",
            output.events
        );
        assert!(
            !side_effect.exists(),
            "{shell}: command-not-found must not enable the && branch"
        );
    }
}

#[test]
fn shell_host_zsh_ambiguous_phrase_stays_in_shell() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-ambiguous-phrase-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-ambiguous-phrase", &work_dir);
    config.native_mode = false;

    let output = run_scripted_zsh(
        &config,
        &[ScriptedInput::user_line(
            "_cosh_test_missing_ambiguous build",
        )],
    )
    .expect("scripted zsh");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(
        terminal.contains("command not found: _cosh_test_missing_ambiguous"),
        "{terminal}"
    );
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("_cosh_test_missing_ambiguous build")
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[test]
fn shell_host_bash_sensitive_missing_emits_raw_free_provenance() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-sensitive-missing-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let input = "missing_sensitive_cli --token=secretvalue";
    let output = run_scripted_bash(
        &ShellHostConfig::new("bash-sensitive-missing", &work_dir),
        &[ScriptedInput::user_line(input)],
    )
    .expect("scripted bash");
    let routing = output
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandRoutingObserved)
        .unwrap_or_else(|| panic!("routing provenance: {:?}", output.events));

    // Post-#2138 the sensitive gate no longer short-circuits ahead of intent
    // classification: `--token=...` vetoes to intent=command, which keeps the
    // native error and emits raw-free provenance with the sensitive flag.
    assert_eq!(routing.component.as_deref(), Some("command"));
    assert!(routing.routing.as_ref().is_some_and(|metadata| {
        metadata.generation == 1
            && metadata.top_level_missing
            && metadata.proven
            && metadata.sensitive
            && !metadata.unsafe_input
    }));
    assert!(routing.input.is_none());
    assert!(routing.command.is_none());
    assert!(!format!("{:?}", output.events).contains("secretvalue"));
    assert!(String::from_utf8_lossy(&output.terminal_output).contains("command not found"));
}

#[test]
fn shell_host_bash_success_then_missing_keeps_routing_provenance() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-success-then-missing-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let output = run_scripted_bash(
        &ShellHostConfig::new("bash-success-then-missing", &work_dir),
        &[
            ScriptedInput::user_line("printf '__PREVIOUS_OK__\\n'"),
            ScriptedInput::user_line("sdsd"),
        ],
    )
    .expect("scripted bash");

    let failed_id = output
        .events
        .iter()
        .find(|event| {
            event.kind == ShellEventKind::CommandFailed && event.command.as_deref() == Some("sdsd")
        })
        .and_then(|event| event.command_id.as_deref())
        .expect("missing command block");
    assert!(
        output.events.iter().any(|event| {
            event.kind == ShellEventKind::CommandRoutingObserved
                && event.command_id.as_deref() == Some(failed_id)
                && event.routing.as_ref().is_some_and(|routing| {
                    routing.top_level_missing && routing.proven && !routing.sensitive
                })
        }),
        "{:?}",
        output.events
    );
}

/// #2138: natural-language input carrying a secret routes to the agent like
/// regular NL (intercept event emitted, sensitive routing flag set) instead
/// of being silently vetoed to the native command-not-found error. The
/// harness returns journal-redacted events, so the intercept input must be
/// the whole-field redaction and the raw key must never appear (V3); the raw
/// text reaching the in-memory agent path is anchored in osc_tests.
#[test]
fn shell_host_sensitive_natural_language_routes_to_agent_with_flag() {
    let issue_input = "帮我安装下openclaw,模型使用qwen3.8-max,API Key: sk-fbaa6";
    let mut shells = Vec::new();
    if bash_supports_command_not_found_handler() {
        shells.push("bash");
    }
    if Command::new("zsh").arg("--version").output().is_ok() {
        shells.push("zsh");
    }
    for shell in shells {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-{shell}-sensitive-nl-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let config = ShellHostConfig::new(format!("{shell}-sensitive-nl"), &work_dir);
        let output = if shell == "bash" {
            run_scripted_bash(&config, &[ScriptedInput::user_line(issue_input)])
        } else {
            run_scripted_zsh(&config, &[ScriptedInput::user_line(issue_input)])
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));

        let terminal = String::from_utf8_lossy(&output.terminal_output);
        assert!(
            !terminal.contains("command not found") && !terminal.contains("未找到命令"),
            "{shell}: {terminal}"
        );
        let intercept = output
            .events
            .iter()
            .find(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.component.as_deref() == Some("natural_language")
            })
            .unwrap_or_else(|| panic!("{shell}: sensitive NL intercept: {:?}", output.events));
        assert_eq!(intercept.input.as_deref(), Some("<redacted>"), "{shell}");
        assert!(
            intercept.routing.as_ref().is_some_and(
                |routing| routing.sensitive && routing.top_level_missing == (shell == "zsh")
            ),
            "{shell}: {intercept:?}"
        );
        assert!(
            !format!("{:?}", output.events).contains("sk-fbaa6"),
            "{shell}: {:?}",
            output.events
        );
        let journal = std::fs::read_to_string(&output.journal_path).unwrap();
        assert!(!journal.contains("sk-fbaa6"), "{shell}: {journal}");
    }
}

#[test]
fn shell_host_missing_cksum_keeps_raw_free_sensitive_provenance() {
    // Post-#2138 the sensitive path shares the regular literal-first-word
    // identity check and no longer depends on the cksum fingerprint, so a
    // broken cksum only degrades the unsafe (invalid UTF-8) path. Sensitive
    // provenance must still be emitted, raw-free, with the native error.
    let input = "missing_sensitive_cli --token=secretvalue";
    let mut shells = Vec::new();
    if bash_supports_command_not_found_handler() {
        shells.push("bash");
    }
    if Command::new("zsh").arg("--version").output().is_ok() {
        shells.push("zsh");
    }
    let mut outputs = Vec::new();
    for shell in shells {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-{shell}-missing-cksum-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let bin_dir = work_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("stub bin dir");
        let cksum = bin_dir.join("cksum");
        std::fs::write(&cksum, "#!/bin/sh\nexit 1\n").expect("cksum stub");
        make_executable(&cksum);
        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let config = ShellHostConfig::new(format!("{shell}-missing-cksum"), &work_dir)
            .with_env("PATH", path);
        let output = if shell == "bash" {
            run_scripted_bash(&config, &[ScriptedInput::user_line(input)])
        } else {
            run_scripted_zsh(&config, &[ScriptedInput::user_line(input)])
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        outputs.push((shell, output));
    }

    for (shell, output) in outputs {
        assert!(output.events.iter().any(|event| {
            event.kind == ShellEventKind::CommandStarted
                && event.command.as_deref() == Some("<redacted sensitive command>")
        }));
        assert!(output.events.iter().any(|event| {
            event.kind == ShellEventKind::CommandFailed && event.exit_code == Some(127)
        }));
        let routing = output
            .events
            .iter()
            .find(|event| event.kind == ShellEventKind::CommandRoutingObserved)
            .unwrap_or_else(|| panic!("{shell}: routing provenance: {:?}", output.events));
        assert_eq!(routing.component.as_deref(), Some("command"), "{shell}");
        assert!(
            routing.routing.as_ref().is_some_and(|metadata| {
                metadata.top_level_missing && metadata.sensitive && !metadata.unsafe_input
            }),
            "{shell}: {routing:?}"
        );
        assert!(routing.input.is_none(), "{shell}");
        assert!(routing.command.is_none(), "{shell}");
        assert!(
            !format!("{:?}", output.events).contains("secretvalue"),
            "{shell}: {:?}",
            output.events
        );
        assert!(
            String::from_utf8_lossy(&output.terminal_output).contains("command not found"),
            "{shell}: {}",
            String::from_utf8_lossy(&output.terminal_output)
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn shell_host_linux_bash_natural_language_routes_directly_to_agent() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-who-are-you-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-who-are-you", &work_dir);
    config.native_mode = false;

    for input in ["Who are you", "Just do it"] {
        let output =
            run_scripted_bash(&config, &[ScriptedInput::user_line(input)]).expect("scripted bash");
        let terminal = String::from_utf8_lossy(&output.terminal_output);

        assert!(!terminal.contains("command not found"), "{terminal}");
        assert!(output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input)
                && event.command_id.is_none()
                && event.component.as_deref() == Some("natural_language")
        }));
        let ledger = build_command_blocks(&output.events);
        assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
        assert!(!ledger.blocks.iter().any(|block| block.command == input));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn shell_host_linux_bash_ambiguous_phrase_stays_in_shell() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-ambiguous-phrase-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-ambiguous-phrase", &work_dir);
    config.native_mode = false;

    let output = run_scripted_bash(
        &config,
        &[ScriptedInput::user_line(
            "_cosh_test_missing_ambiguous build",
        )],
    )
    .expect("scripted bash");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(terminal.contains("command not found"), "{terminal}");
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("_cosh_test_missing_ambiguous build")
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[cfg(target_os = "linux")]
#[test]
fn shell_host_linux_bash_ignores_inherited_system_missing_handler() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-system-missing-handler-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-system-missing-handler", &work_dir).with_env(
        "BASH_FUNC_command_not_found_handle%%",
        "() { printf '__system_handler__\\n'; return 127; }",
    );
    config.native_mode = false;

    let output = run_scripted_bash(&config, &[ScriptedInput::user_line("Who are you")])
        .expect("scripted bash");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(!terminal.contains("__system_handler__"), "{terminal}");
    assert!(!terminal.contains("command not found"), "{terminal}");
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("Who are you")
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[test]
fn shell_host_zsh_ai_disabled_keeps_missing_natural_language_in_shell() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-ai-disabled-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-ai-disabled", &work_dir).with_ai_enabled(false);
    config.native_mode = false;

    let output = run_scripted_zsh(&config, &[ScriptedInput::user_line("Kindly explain this")])
        .expect("scripted zsh");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(terminal.contains("command not found: Kindly"), "{terminal}");
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[test]
fn shell_host_zsh_nested_missing_is_not_treated_as_top_level_input() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-nested-missing-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(home_dir.join(".zshrc"), "ask() { please explain this; }\n").expect("zshrc");
    let config = ShellHostConfig::new("zsh-nested-missing", &work_dir)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("COSH_ZDOTDIR_ORIG", home_dir.display().to_string());

    let output =
        run_scripted_zsh(&config, &[ScriptedInput::user_line("ask")]).expect("scripted zsh");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(terminal.contains("command not found: please"), "{terminal}");
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[test]
fn shell_host_bash_preserves_user_missing_handler_contract() {
    if Command::new("bash").arg("--version").output().is_err()
        || !bash_supports_command_not_found_handler()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-user-missing-handler-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "command_not_found_handle() {\n\
         printf '__user_handler__:%s:%s\\n' \"$#\" \"$*\"\n\
         handler_inner_missing\n\
         return 42\n\
         }\n",
    )
    .expect("bashrc");
    let config = ShellHostConfig::new("bash-user-missing-handler", &work_dir)
        .with_env("HOME", home_dir.display().to_string());

    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("terraform plan"),
            ScriptedInput::user_line("please explain this"),
        ],
    )
    .expect("scripted bash");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(
        terminal.contains("__user_handler__:2:terraform plan"),
        "{terminal}"
    );
    assert!(terminal.contains("handler_inner_missing"), "{terminal}");
    assert!(
        terminal.contains("__user_handler__:3:please explain this"),
        "{terminal}"
    );
    let ledger = ledger_from_output(&output);
    for command in ["terraform plan", "please explain this"] {
        let block = ledger
            .blocks
            .iter()
            .find(|block| block.command == command)
            .unwrap_or_else(|| panic!("{command} block"));
        assert_eq!(block.exit_code, 42, "{terminal}\n{:?}", output.events);
    }
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[test]
fn shell_host_zsh_preserves_user_missing_handler_contract() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-user-missing-handler-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".zshrc"),
        "command_not_found_handler() {\n\
         printf '__user_handler__:%s:%s\\n' \"$#\" \"$*\"\n\
         handler_inner_missing\n\
         return 42\n\
         }\n",
    )
    .expect("zshrc");
    let config = ShellHostConfig::new("zsh-user-missing-handler", &work_dir)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("COSH_ZDOTDIR_ORIG", home_dir.display().to_string());

    let output = run_scripted_zsh(
        &config,
        &[
            ScriptedInput::user_line("terraform plan"),
            ScriptedInput::user_line("please explain this"),
        ],
    )
    .expect("scripted zsh");
    let terminal = String::from_utf8_lossy(&output.terminal_output);

    assert!(
        terminal.contains("__user_handler__:2:terraform plan"),
        "{terminal}"
    );
    assert!(
        terminal.contains("command not found: handler_inner_missing"),
        "{terminal}"
    );
    assert!(
        terminal.contains("__user_handler__:3:please explain this"),
        "{terminal}"
    );
    let ledger = ledger_from_output(&output);
    for command in ["terraform plan", "please explain this"] {
        let block = ledger
            .blocks
            .iter()
            .find(|block| block.command == command)
            .unwrap_or_else(|| panic!("{command} block"));
        assert_eq!(block.exit_code, 42, "{terminal}\n{:?}", output.events);
        assert!(output.events.iter().any(|event| {
            event.kind == ShellEventKind::CommandRoutingObserved
                && event.command_id.as_deref() == Some(block.id.as_str())
                && event.routing.as_ref().is_some_and(|routing| routing.proven)
        }));
    }
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[test]
fn shell_host_preserves_native_prompt_command_output() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-prompt-command-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(home_dir.join(".bash_history"), "exit\n").expect("history");
    std::fs::write(
        home_dir.join(".bashrc"),
        "set -o history\n\
         HISTFILE=\"$HOME/.bash_history\"\n\
         history -r \"$HISTFILE\" 2>/dev/null || true\n\
         PROMPT_COMMAND='PATH=\"/prompt-hook:$PATH\"; history 1 >/dev/null; printf \"__cosh_prompt_noise__\\n\" >&2'\n",
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("prompt-command-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[ScriptedInput::user_line("ls /path/that/does/not/exist")],
    )
    .expect("scripted bash pty");

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    let failed = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("/path/that/does/not/exist"))
        .expect("failed command block");
    assert_ne!(failed.exit_code, 0);
    assert_eq!(failed.shell_environment_generation, None);
    let output_ref = failed
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let output_ref_text = std::fs::read_to_string(output_ref).expect("output ref text");
    // Cosh no longer wraps PROMPT_COMMAND to manufacture an earlier boundary.
    // Bash-native prompt output therefore remains part of the preceding
    // command's observable terminal stream.
    assert!(output_ref_text.contains("__cosh_prompt_noise__"));
}

#[test]
fn shell_host_bash_tracks_native_history_file_changes() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-history-file-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    let relative_one = work_dir.join("relative-one");
    let relative_two = work_dir.join("relative-two");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::create_dir_all(&relative_one).expect("first relative dir");
    std::fs::create_dir_all(&relative_two).expect("second relative dir");

    let initial_history = home_dir.join("initial-history");
    let alternate_history = home_dir.join("alternate-history");
    let observed_history_files = work_dir.join("observed-history-files");
    std::fs::write(
        home_dir.join(".bashrc"),
        format!("export HISTFILE={}\n", shell_arg(&initial_history)),
    )
    .expect("bashrc");

    let install_marker_sink = format!(
        "_cosh_native_history_file_fragment() {{ \
         local history_file; \
         history_file=$(_cosh_native_history_file_path) || return 0; \
         printf '%s\\n' \"$history_file\" >> {}; \
         }}",
        shell_arg(&observed_history_files)
    );
    let config = ShellHostConfig::new("history-file-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(install_marker_sink),
            ScriptedInput::user_line("echo unchanged-history-file"),
            ScriptedInput::user_line(format!("export HISTFILE={}", shell_arg(&alternate_history))),
            ScriptedInput::user_line("echo unchanged-alternate-history-file"),
            ScriptedInput::user_line(format!(
                "cd {}; export HISTFILE=relative-history",
                shell_arg(&relative_one)
            )),
            ScriptedInput::user_line(format!("cd {}", shell_arg(&relative_two))),
            ScriptedInput::user_line("false"),
        ],
    )
    .expect("scripted bash pty");

    let mut observed = std::fs::read_to_string(&observed_history_files)
        .expect("observed history files")
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    observed.dedup();
    let expected = [
        initial_history,
        alternate_history,
        relative_one.join("relative-history"),
        relative_two.join("relative-history"),
    ]
    .into_iter()
    .map(|path| path.display().to_string())
    .collect::<Vec<_>>();
    assert_eq!(observed, expected);

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    let failed = ledger
        .blocks
        .iter()
        .find(|block| block.command == "false")
        .expect("false command block");
    assert_eq!(failed.exit_code, 1);
}

#[test]
fn shell_host_bash_tracks_history_file_changed_by_prompt_command() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-prompt-history-file-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");

    let initial_history = home_dir.join("initial-history");
    let prompt_history = home_dir.join("prompt-history");
    let observed_history_files = work_dir.join("observed-history-files");
    std::fs::write(
        home_dir.join(".bashrc"),
        format!(
            "export HISTFILE={}\n\
             export COSH_PROMPT_HISTORY_FILE={}\n\
             PROMPT_COMMAND='if [[ \"${{COSH_SWITCH_HISTORY:-}}\" == 1 ]]; then \
             HISTFILE=\"$COSH_PROMPT_HISTORY_FILE\"; unset COSH_SWITCH_HISTORY; fi'\n",
            shell_arg(&initial_history),
            shell_arg(&prompt_history)
        ),
    )
    .expect("bashrc");

    let install_marker_sink = format!(
        "_cosh_native_history_file_fragment() {{ \
         local history_file; \
         history_file=$(_cosh_native_history_file_path) || return 0; \
         printf '%s\\n' \"$history_file\" >> {}; \
         }}",
        shell_arg(&observed_history_files)
    );
    let config = ShellHostConfig::new("prompt-history-file-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(install_marker_sink),
            ScriptedInput::user_line("export COSH_SWITCH_HISTORY=1"),
        ],
    )
    .expect("scripted bash pty");

    let mut observed = std::fs::read_to_string(&observed_history_files)
        .expect("observed history files")
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    observed.dedup();
    assert_eq!(
        observed,
        vec![
            initial_history.display().to_string(),
            prompt_history.display().to_string(),
        ]
    );
}

#[test]
fn shell_host_bash_isolated_mode_omits_history_file_markers() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-isolated-history-file-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let observed_history_files = work_dir.join("observed-history-files");
    let install_marker_sink = format!(
        "_COSH_LAST_NATIVE_HISTORY_FILE=; \
         _cosh_emit_native_history_file_marker() {{ \
         printf '%s\\n' \"$1\" >> {}; \
         }}",
        shell_arg(&observed_history_files)
    );
    let mut config = ShellHostConfig::new("isolated-history-file-test", &work_dir);
    config.native_mode = false;

    run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(install_marker_sink),
            ScriptedInput::user_line("export HISTFILE=/tmp/isolated-history"),
        ],
    )
    .expect("scripted isolated bash pty");

    assert!(!observed_history_files.exists());
}

#[test]
fn shell_host_rejects_forged_osc_markers_without_session_token() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-forged-osc-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");

    fn forged_marker(event: &str, token: Option<&str>, command: &str) -> String {
        let token_field = token
            .map(|token| format!(r#","token":"{token}""#))
            .unwrap_or_default();
        let reason_field = if event == "intercept" {
            r#","reason":"natural_language""#
        } else {
            ""
        };
        format!(
            r#"printf '\033]1337;COSH;{{"event":"{event}"{token_field},"session_id":"forged","timestamp_ms":1,"cwd":"/tmp","command":"{command}"{reason_field},"status":0}}\a'"#
        )
    }

    let forged_marker_inputs = ["preexec", "precmd", "intercept"]
        .into_iter()
        .flat_map(|event| {
            [
                forged_marker(event, None, &format!("echo forged-{event}-missing-token")),
                forged_marker(
                    event,
                    Some("wrong"),
                    &format!("echo forged-{event}-wrong-token"),
                ),
            ]
        })
        .map(ScriptedInput::user_line);
    let split_marker = "printf '\\033]1337;COSH;{\"event\":\"preexec\",\"session_id\":\"forged\",\"timestamp_ms\":1,'; printf '\"cwd\":\"/tmp\",\"command\":\"echo forged-split-token\",\"status\":0}\\a'";

    let config = ShellHostConfig::new("forged-osc-test", &work_dir);
    let scripted_inputs: Vec<_> = forged_marker_inputs
        .chain([
            ScriptedInput::user_line(split_marker),
            ScriptedInput::user_line("echo real-after-forge"),
        ])
        .collect();
    let output = run_scripted_bash(&config, &scripted_inputs).expect("scripted bash pty");

    assert_no_osc_marker(&output.terminal_output);
    assert!(!output.events.iter().any(|event| {
        matches!(
            event.kind,
            ShellEventKind::CommandStarted
                | ShellEventKind::CommandCompleted
                | ShellEventKind::UserInputIntercepted
                | ShellEventKind::ShellReady
        ) && (event.session_id == "forged"
            || event
                .command
                .as_deref()
                .is_some_and(|command| command.starts_with("echo forged-"))
            || event
                .input
                .as_deref()
                .is_some_and(|input| input.starts_with("echo forged-")))
    }));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted
            && event.command.as_deref() == Some("echo real-after-forge")
    }));
}

#[test]
fn shell_host_pty_cannot_read_private_marker_token() {
    // Issue #2413, live-PTY verification: the unit tests feed the parser
    // directly, but the issue explicitly asks for live-PTY evidence. A real
    // scripted bash session prints a syntactically valid, token-carrying
    // precmd marker whose `status` field is omitted (the protocol-drift /
    // truncation shape) while that printf's own preexec is still in flight;
    // the host must finish the command as Failed with the -1 missing-exit
    // sentinel instead of fabricating success. The `toke""n` split keeps
    // the literal line out of the marker script's secret-redaction patterns
    // while the shell still prints a well-formed `"token"` key, and
    // "$COSH_MARKER_TOKEN" expands the session's real token so the
    // parser's exact-match check accepts the sequence.
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-pty-precmd-driftless-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");

    let statusless_injection = "printf \"\\033]1337;COSH;{\\\"event\\\":\\\"precmd\\\",\\\"toke\"\"n\\\":\\\"%s\\\",\\\"cwd\\\":\\\"/tmp/driftless-probe\\\"}\\a\" \"$COSH_MARKER_TOKEN\"";

    // The genuine-chain control runs BEFORE the injection: once the forged
    // precmd has been consumed mid-command, the marker script's own
    // history/DEBUG-trap state machine treats follow-up queued lines
    // differently, so the control must not depend on post-injection input.
    // Isolated mode keeps the host-environment PROMPT_COMMAND/PS1 (e.g. an
    // audit hook plus a backticked `pwd` prompt on bash 4.2) out of the
    // session: those make the marker script's DEBUG-trap snapshot logic
    // clear _COSH_ACTIVE_DEBUG_TRAP after the first prompt, silencing every
    // later preexec marker and drowning the assertion under environment
    // noise unrelated to #2413.
    let mut config = ShellHostConfig::new("pty-precmd-driftless", &work_dir);
    config.native_mode = false;
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("echo drift-real-probe"),
            ScriptedInput::user_line(statusless_injection),
        ],
    )
    .expect("scripted bash pty");

    // The public token variable is absent, so the forged marker is rejected
    // and the printf completes through the genuine bounded marker chain.
    let drifted = output
        .events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
            ) && event
                .command
                .as_deref()
                .is_some_and(|command| command.contains("driftless-probe"))
        })
        .expect("drifted injection command finish event");
    assert_eq!(drifted.kind, ShellEventKind::CommandCompleted);
    assert_eq!(drifted.exit_code, Some(0));

    // Control: the genuine marker chain keeps reporting real completions
    // after the drifted sequence.
    let real = output
        .events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
            ) && event
                .command
                .as_deref()
                .is_some_and(|command| command.contains("drift-real-probe"))
        })
        .expect("real command finish event");
    assert_eq!(real.kind, ShellEventKind::CommandCompleted);
    assert_eq!(real.exit_code, Some(0));

    // The ledger agrees that no injected status reached the parser.
    let ledger = build_command_blocks(&output.events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    let drifted_block = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("driftless-probe"))
        .expect("drifted command block");
    assert_eq!(drifted_block.exit_code, 0);
    assert_eq!(
        drifted_block.status,
        cosh_shell::types::CommandStatus::Completed
    );

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn shell_host_pty_statusful_precmd_marker_completes_inflight_command() {
    // Control for the #2413 live-PTY verification: the same injection
    // mechanism carrying an explicit status:0 must finish the in-flight
    // printf as Completed/0 on both the fixed and the pre-fix code — this
    // proves the drift verdict above comes from the missing status field,
    // not from the injection harness itself.
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-pty-precmd-driftzero-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");

    let status_zero_injection = "printf \"\\033]1337;COSH;{\\\"event\\\":\\\"precmd\\\",\\\"toke\"\"n\\\":\\\"%s\\\",\\\"cwd\\\":\\\"/tmp/drift-zero-probe\\\",\\\"status\\\":0}\\a\" \"$COSH_MARKER_TOKEN\"";

    // Isolated mode, same rationale as the driftless case above.
    let mut config = ShellHostConfig::new("pty-precmd-driftzero", &work_dir);
    config.native_mode = false;
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("echo drift-real-probe"),
            ScriptedInput::user_line(status_zero_injection),
        ],
    )
    .expect("scripted bash pty");

    let status_zero = output
        .events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
            ) && event
                .command
                .as_deref()
                .is_some_and(|command| command.contains("drift-zero-probe"))
        })
        .expect("status-zero injection command finish event");
    assert_eq!(status_zero.kind, ShellEventKind::CommandCompleted);
    assert_eq!(status_zero.exit_code, Some(0));

    let real = output
        .events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
            ) && event
                .command
                .as_deref()
                .is_some_and(|command| command.contains("drift-real-probe"))
        })
        .expect("real command finish event");
    assert_eq!(real.kind, ShellEventKind::CommandCompleted);
    assert_eq!(real.exit_code, Some(0));

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn shell_host_zsh_adapter_emits_shared_command_events() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-host-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let unicode_file = work_dir.join("\u{8bbe}\u{8ba1}\u{6587}\u{6863}.md");
    std::fs::write(&unicode_file, "\u{4e2d}\u{6587}\u{5185}\u{5bb9}").expect("unicode file");

    let config = ShellHostConfig::new("zsh-host-test", &work_dir);
    let output = run_scripted_zsh(
        &config,
        &[
            ScriptedInput::user_line("/help"),
            ScriptedInput::user_line("echo zsh-ok"),
            ScriptedInput::user_line(format!("cat {}", shell_arg(&unicode_file))),
            ScriptedInput::user_line("ls /path/that/does/not/exist"),
        ],
    )
    .expect("scripted zsh pty");

    assert_no_osc_marker(&output.terminal_output);
    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(
        output
            .events
            .iter()
            .any(|event| event.kind == ShellEventKind::ShellStarted),
        "{terminal}\n{:?}",
        output.events
    );
    assert!(
        output
            .events
            .iter()
            .any(|event| event.kind == ShellEventKind::ShellReady),
        "{terminal}\n{:?}",
        output.events
    );
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("/help")
            && event.component.as_deref() == Some("slash")
    }));

    let ledger = ledger_from_output(&output);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo zsh-ok") && block.exit_code == 0));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("cat ") && block.exit_code == 0));
    assert!(ledger.blocks.iter().any(|block| {
        block.command.contains("/path/that/does/not/exist") && block.exit_code != 0
    }));
    assert!(ledger
        .blocks
        .iter()
        .filter(|block| block.command.contains("zsh-ok") || block.command.contains("cat "))
        .all(|block| block.shell_environment_generation.is_some()));
}

#[test]
fn routing_c4_zsh_stubs_intercept_bypass_only_commands() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c4-zsh-stubs-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c4-zsh-stubs", &work_dir);
    let inputs = ["/resume", "/session", "/task"];
    let steps = inputs
        .iter()
        .map(|input| ScriptedInput::user_line(*input))
        .collect::<Vec<_>>();
    let output = run_scripted_zsh(&config, &steps).expect("scripted zsh slash stubs");

    for input in inputs {
        assert!(output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input)
                && event.component.as_deref() == Some("slash")
        }));
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::CommandStarted && event.command.as_deref() == Some(input)
        }));
    }
}

#[test]
fn shell_host_zsh_later_preexec_hook_fails_closed_for_path_generation() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-path-trust-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let config = ShellHostConfig::new("zsh-path-trust-test", &work_dir);
    let output = run_scripted_zsh(
        &config,
        &[
            ScriptedInput::user_line("function _cosh_test_later_preexec { PATH=/later:$PATH }"),
            ScriptedInput::user_line("add-zsh-hook preexec _cosh_test_later_preexec"),
            ScriptedInput::user_line("echo after-later-hook"),
        ],
    )
    .expect("scripted zsh pty");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == "echo after-later-hook")
        .expect("command after later preexec hook");
    assert_eq!(block.shell_environment_generation, None);
}

#[test]
fn shell_host_bash_combined_debug_trap_fails_closed_for_path_generation() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-path-trust-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let config = ShellHostConfig::new("bash-path-trust-test", &work_dir);
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("trap '_cosh_preexec_marker; :' DEBUG"),
            ScriptedInput::user_line("echo after-combined-trap"),
        ],
    )
    .expect("scripted bash pty");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == "echo after-combined-trap")
        .expect("command after combined DEBUG trap");
    assert_eq!(block.shell_environment_generation, None);
}

#[test]
fn shell_host_bash_captured_debug_trap_keeps_path_generation_trusted() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-captured-trap-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "trap 'PATH=/captured:$PATH' DEBUG\n",
    )
    .expect("bashrc");
    let config = ShellHostConfig::new("bash-captured-trap-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[ScriptedInput::user_line("echo after-captured-trap")],
    )
    .expect("scripted bash pty");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == "echo after-captured-trap")
        .expect("command after captured DEBUG trap");
    assert_eq!(block.shell_environment_generation, None);
}

#[test]
fn shell_host_bash_preserves_bashopts_without_enabling_extdebug() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    // BASHOPTS environment import exists since bash 4.1; on older hosts
    // (e.g. macOS /bin/bash 3.2) the leak vector cannot exist, so skip.
    let bashopts_supported = Command::new("bash")
        .env("BASHOPTS", "cdspell")
        .args(["--noprofile", "--norc", "-c", "shopt -q cdspell"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !bashopts_supported {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bashopts-unexport-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    // Child probe reporting whether extdebug leaked into a fresh bash. The
    // rc markers are expanded at runtime so the echoed input line can never
    // satisfy the assertions by itself.
    let probe_path = work_dir.join("bashopts-probe.sh");
    std::fs::write(
        &probe_path,
        "#!/bin/bash\nshopt -q extdebug; echo \"child-extdebug-rc=$?\"\n",
    )
    .expect("probe script");
    make_executable(&probe_path);

    // BASHOPTS arrives exported from the environment: bash keeps the export
    // attribute, which is exactly the leak precondition from issue #1782.
    let config =
        ShellHostConfig::new("bashopts-unexport-test", &work_dir).with_env("BASHOPTS", "cdspell");
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("shopt -q extdebug; echo \"host-extdebug-rc=$?\""),
            ScriptedInput::user_line("shopt -q cdspell; echo \"host-cdspell-rc=$?\""),
            ScriptedInput::user_line(
                "attrs=\"$(declare -p BASHOPTS)\"; attrs=\"${attrs%%BASHOPTS*}\"; \
                 [[ \"$attrs\" == *x* ]]; echo \"bashopts-export-rc=$?\"",
            ),
            ScriptedInput::user_line(format!("bash {}", shell_arg(&probe_path))),
        ],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    // Enhanced preserves both the user's imported option and its export
    // attribute while leaving extdebug at the user's actual setting.
    assert!(terminal.contains("host-extdebug-rc=1"), "{terminal}");
    assert!(terminal.contains("host-cdspell-rc=0"), "{terminal}");
    assert!(terminal.contains("bashopts-export-rc=0"), "{terminal}");
    // A child bash spawned from the session must not start in extdebug mode
    // and must not trip the bashdb debugger-profile load.
    assert!(terminal.contains("child-extdebug-rc=1"), "{terminal}");
    assert!(!terminal.contains("bashdb"), "{terminal}");
}

#[test]
fn shell_host_bash_debug_trap_children_never_see_exported_extdebug() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    // Same BASHOPTS-import gate as the leak test above.
    let bashopts_supported = Command::new("bash")
        .env("BASHOPTS", "cdspell")
        .args(["--noprofile", "--norc", "-c", "shopt -q cdspell"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !bashopts_supported {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bashopts-trap-window-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    // A user rcfile runs before the marker's hook setup, so its DEBUG trap
    // is live while the marker enables extdebug. The trap records every
    // BASHOPTS frame and pipes child-bash stderr into evidence files; the
    // trailing ':' keeps the handler's exit status at 0 so it can never
    // suppress commands under extdebug. The leak detector is the exported
    // extdebug frame plus the child's bashdb load failure, because a leaking
    // child disables extdebug before shopt state could be probed.
    std::fs::write(
        home_dir.join(".bashrc"),
        "trap 'declare -p BASHOPTS >> \"$HOME/trap-log\" 2>/dev/null; bash -c \":\" 2>> \"$HOME/trap-err\"; :' DEBUG\n",
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("bashopts-trap-window-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("BASHOPTS", "cdspell");
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("printf 'shell-alive-%s\\n' ok"),
            ScriptedInput::user_line(
                "leak=trap-child-extdebug-leaked; clean=window-clean; \
                 if grep -q \"^declare -[^ ]*x[^ ]* BASHOPTS=.*extdebug\" \"$HOME/trap-log\" || \
                    [[ -s \"$HOME/trap-err\" ]]; \
                 then echo \"__${leak}__\"; else echo \"trap-${clean}-ok\"; fi",
            ),
        ],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    // The marker must drop the BASHOPTS export attribute before enabling
    // extdebug, so no DEBUG trap firing in between can leak it to children.
    // Markers only appear after runtime expansion, so the echoed input line
    // cannot satisfy either assertion by itself.
    assert!(
        !terminal.contains("__trap-child-extdebug-leaked__"),
        "{terminal}"
    );
    assert!(terminal.contains("trap-window-clean-ok"), "{terminal}");
    assert!(terminal.contains("shell-alive-ok"), "{terminal}");
}

// bash re-execs shebang-less scripts with --debugger only when built with
// debugger support, and the startup failure is only visible on hosts without
// the bashdb package. Probe the exact re-exec path so tests can skip
// anywhere the regression cannot manifest (e.g. macOS bash 3.2).
fn bash_reexecs_shebang_less_with_debugger(work_dir: &std::path::Path) -> bool {
    let probe_path = work_dir.join("shebang-less-probe");
    std::fs::write(&probe_path, "true\n").expect("probe script");
    make_executable(&probe_path);
    let probe = Command::new("bash")
        .args(["--noprofile", "--norc", "-c"])
        .arg(format!("shopt -s extdebug; {}", probe_path.display()))
        .output()
        .expect("debugger probe");
    let stderr = String::from_utf8_lossy(&probe.stderr);
    stderr.contains("bashdb") || stderr.contains("cannot start debugger")
}

#[test]
fn shell_host_bash_shebang_less_prompt_hook_avoids_debugger_reexec() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-prompt-hook-debugger-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");

    if !bash_reexecs_shebang_less_with_debugger(&work_dir) {
        return;
    }

    // Hook mirroring Alinux /etc/sysconfig/bash-prompt-history: inherited
    // PROMPT_COMMAND points at a shebang-less executable script, so the
    // marker's eval can only run it through the ENOEXEC re-exec fallback.
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    let hook_log = work_dir.join("hook-ran.log");
    let hook_path = work_dir.join("bash-prompt-history");
    std::fs::write(
        &hook_path,
        format!("echo prompt-hook-ran >> {}\n", hook_log.display()),
    )
    .expect("hook script");
    make_executable(&hook_path);

    let config = ShellHostConfig::new("prompt-hook-debugger-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("PROMPT_COMMAND", hook_path.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("echo ok"),
            ScriptedInput::user_line("shopt -q extdebug; echo \"post-hook-extdebug-rc=$?\""),
        ],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    // The hook must still run — the fix silences the debugger re-exec,
    // not the user prompt command.
    let hook_ran = std::fs::read_to_string(&hook_log).unwrap_or_default();
    assert!(hook_ran.contains("prompt-hook-ran"), "{terminal}");
    assert!(!terminal.contains("bashdb"), "{terminal}");
    assert!(!terminal.contains("cannot start debugger"), "{terminal}");
    // extdebug must be back on for the next real command: the DEBUG trap
    // return-1 suppression depends on it.
    assert!(terminal.contains("post-hook-extdebug-rc=1"), "{terminal}");
}

#[test]
fn shell_host_bash_shebang_less_prompt_hook_array_form_avoids_debugger_reexec() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    // Bash executes every PROMPT_COMMAND array element only since 5.1.
    let version_probe = Command::new("bash")
        .args(["-c", "echo ${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}"])
        .output();
    let Ok(version_probe) = version_probe else {
        return;
    };
    let version = String::from_utf8_lossy(&version_probe.stdout);
    let mut parts = version
        .split_whitespace()
        .filter_map(|part| part.parse::<u32>().ok());
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    if (major, minor) < (5, 1) {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-prompt-hook-array-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");

    if !bash_reexecs_shebang_less_with_debugger(&work_dir) {
        return;
    }

    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    let hook_log = work_dir.join("hook-ran.log");
    let hook_path = work_dir.join("bash-prompt-history");
    std::fs::write(
        &hook_path,
        format!("echo array-hook-ran >> {}\n", hook_log.display()),
    )
    .expect("hook script");
    make_executable(&hook_path);
    // Array-form PROMPT_COMMAND captured from the user rcfile. The leading
    // `return 0` element pins the eval helper frame: an early-returning hook
    // must not skip the remaining elements or the extdebug restore.
    std::fs::write(
        home_dir.join(".bashrc"),
        format!("PROMPT_COMMAND=('return 0' '{}')\n", hook_path.display()),
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("prompt-hook-array-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("echo ok"),
            ScriptedInput::user_line("shopt -q extdebug; echo \"post-hook-extdebug-rc=$?\""),
        ],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    let hook_ran = std::fs::read_to_string(&hook_log).unwrap_or_default();
    assert!(hook_ran.contains("array-hook-ran"), "{terminal}");
    assert!(!terminal.contains("bashdb"), "{terminal}");
    assert!(!terminal.contains("cannot start debugger"), "{terminal}");
    assert!(terminal.contains("post-hook-extdebug-rc=1"), "{terminal}");
}

fn bash_extdebug_clears_trace_options() -> bool {
    // `shopt -u extdebug` also clears errtrace/functrace on every bash that
    // implements the implication (4.4 through 5.x); skip only where bash is
    // too old to link the flags at all.
    Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "set -E; set -T; shopt -s extdebug; shopt -u extdebug; \
             [[ ! -o errtrace && ! -o functrace ]]",
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
fn shell_host_bash_prompt_hook_observes_only_bounded_capture() {
    if !bash_extdebug_clears_trace_options() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-prompt-hook-trace-options-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    let trace_log = work_dir.join("trace-options.log");
    std::fs::write(
        home_dir.join(".bashrc"),
        format!(
            "trap 'printf \"ERR\\t%s\\t%s\\n\" \"$BASH_COMMAND\" \
             \"${{FUNCNAME[*]}}\" >> {trace_log}' ERR\n\
             trap 'printf \"DEBUG\\t%s\\t%s\\n\" \"$BASH_COMMAND\" \
             \"${{FUNCNAME[*]}}\" >> {trace_log}' DEBUG\n\
             _user_hook() {{\n\
               false\n\
             }}\n\
             PROMPT_COMMAND=_user_hook\n",
            trace_log = trace_log.display()
        ),
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("prompt-hook-trace-options-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[ScriptedInput::user_line("echo trace-options-preserved")],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    let trace_log = std::fs::read_to_string(&trace_log).unwrap_or_default();
    assert!(
        trace_log.contains("ERR\tfalse\t"),
        "{terminal}\n{trace_log}"
    );
    assert!(
        trace_log.contains("DEBUG\tfalse\t"),
        "{terminal}\n{trace_log}"
    );
    for line in trace_log
        .lines()
        .filter(|line| line.contains("_cosh") || line.contains("_COSH"))
    {
        let mut fields = line.splitn(3, '\t');
        assert_eq!(fields.next(), Some("DEBUG"), "{line}\n{trace_log}");
        let command = fields.next().unwrap_or_default();
        let function_stack = fields.next().unwrap_or_default();
        assert!(
            command.starts_with("_COSH_PROMPT_STATUS=")
                || command.starts_with("_COSH_PROMPT_DEBUG_TRAP=")
                || command.starts_with("_COSH_PROMPT_RETURN_TRAP=")
                || command.starts_with("_COSH_PROMPT_ERR_TRAP=")
                || command == "_COSH_PROMPT_XTRACE=0"
                || command == "_COSH_PROMPT_XTRACE=1"
                || command == "(( _COSH_PROMPT_XTRACE == 1 ))"
                || command == "unset _COSH_PROMPT_XTRACE"
                || (command.starts_with("trap -p DEBUG >")
                    && command.contains("COSH_RECOVERY_REQUEST_FILE")),
            "unexpected Cosh internals in user trap: {line}\n{trace_log}"
        );
        assert!(
            function_stack.is_empty(),
            "user trap entered a Cosh function: {line}\n{trace_log}"
        );
    }
    assert!(terminal.contains("trace-options-preserved"), "{terminal}");
}

#[test]
fn shell_host_bash_prompt_hook_does_not_enable_extdebug() {
    if !bash_extdebug_clears_trace_options() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-prompt-hook-debug-trap-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    // A hook that installs a DEBUG trap ending in `return 0` unwinds every
    // function frame past the in-function extdebug restore. The session must
    // stay usable and extdebug must self-heal after the user clears the trap.
    std::fs::write(
        home_dir.join(".bashrc"),
        "_user_hook() {\n\
           if [[ -z \"${_poisoned:-}\" ]]; then\n\
             _poisoned=1\n\
             trap 'return 0' DEBUG\n\
           fi\n\
         }\n\
         PROMPT_COMMAND=_user_hook\n",
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("prompt-hook-debug-trap-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[
            // Executes natively while the poisoned trap is live: with
            // extdebug off the top-level `return` only prints an error and
            // the command still runs — the session is not bricked.
            ScriptedInput::user_line("trap - DEBUG; echo session-usable"),
            // The prompt cycle after the trap removal ran the in-function
            // restore again, so extdebug must be back on.
            ScriptedInput::user_line("shopt -q extdebug; echo \"post-heal-extdebug-rc=$?\""),
        ],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("session-usable"), "{terminal}");
    assert!(terminal.contains("post-heal-extdebug-rc=1"), "{terminal}");
}

#[test]
fn shell_host_bash_alias_expanded_commands_keep_preexec_markers() {
    // BASH_ALIASES (bash 4+) is required for the alias-aware guard; on
    // older bash (e.g. macOS /bin/bash 3.2) the guard degrades to pre-fix
    // behavior by design, so this test only runs on bash 4+.
    let version_probe = Command::new("bash")
        .args(["-c", "echo ${BASH_VERSINFO[0]}"])
        .output();
    let Ok(version_probe) = version_probe else {
        return;
    };
    let major = String::from_utf8_lossy(&version_probe.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or(0);
    if major < 4 {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-alias-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    let list_dir = work_dir.join("listing");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::create_dir_all(&list_dir).expect("list dir");
    let data_file = work_dir.join("data.txt");
    std::fs::write(&data_file, "needle\n").expect("data file");
    std::fs::write(
        home_dir.join(".bashrc"),
        "alias ls='ls --color=auto'\n\
         alias ll='ls -l'\n\
         alias lg='grep --color=auto -n'\n\
         alias wrap='env '\n",
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("bash-alias-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let single = format!("ls {}", shell_arg(&list_dir));
    let chained = format!("ll {}", shell_arg(&list_dir));
    let assignment_prefixed = format!("FOO=1 ls {}", shell_arg(&list_dir));
    let compound = format!("ls {}; pwd", shell_arg(&list_dir));
    let pipeline = format!("ls {} | wc -l", shell_arg(&list_dir));
    let quoted_alias = format!("lg needle {}", shell_arg(&data_file));
    // Bash keeps alias-expanding the next word when an alias value ends
    // with a blank (alias wrap='env '), so `wrap ll <dir>` really runs
    // `env ls -l <dir>` and the guard must match that expansion.
    let trailing_blank = format!("wrap ll {}", shell_arg(&list_dir));
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(single.clone()),
            ScriptedInput::user_line(chained.clone()),
            ScriptedInput::user_line(assignment_prefixed.clone()),
            ScriptedInput::user_line(compound.clone()),
            ScriptedInput::user_line(pipeline.clone()),
            ScriptedInput::user_line(quoted_alias.clone()),
            ScriptedInput::user_line(trailing_blank.clone()),
        ],
    )
    .expect("scripted bash pty");

    let ledger = ledger_from_output(&output);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    for expected in [
        &single,
        &chained,
        &assignment_prefixed,
        &compound,
        &pipeline,
        &quoted_alias,
        &trailing_blank,
    ] {
        let block = ledger
            .blocks
            .iter()
            .find(|block| block.command == **expected)
            .unwrap_or_else(|| {
                panic!(
                    "missing command block for {expected:?}; blocks: {:?}",
                    ledger
                        .blocks
                        .iter()
                        .map(|block| block.command.as_str())
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(block.exit_code, 0, "{expected}");
    }
    // preexec must report the history original text, never the
    // alias-expanded variant.
    assert!(
        ledger
            .blocks
            .iter()
            .all(|block| !block.command.contains("--color=auto")),
        "{:?}",
        ledger
            .blocks
            .iter()
            .map(|block| block.command.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn shell_host_bash_alias_guard_survives_polluted_ifs() {
    // N6: the alias-aware guard must stay builtin-only and IFS-independent.
    let version_probe = Command::new("bash")
        .args(["-c", "echo ${BASH_VERSINFO[0]}"])
        .output();
    let Ok(version_probe) = version_probe else {
        return;
    };
    let major = String::from_utf8_lossy(&version_probe.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or(0);
    if major < 4 {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-ifs-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    let list_dir = work_dir.join("listing");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::create_dir_all(&list_dir).expect("list dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "alias ls='ls --color=auto'\nIFS=':'\n",
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("bash-ifs-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let aliased = format!("ls {}", shell_arg(&list_dir));
    let output = run_scripted_bash(&config, &[ScriptedInput::user_line(aliased.clone())])
        .expect("scripted bash pty");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == aliased)
        .unwrap_or_else(|| {
            panic!(
                "missing aliased block under polluted IFS; blocks: {:?}",
                ledger
                    .blocks
                    .iter()
                    .map(|block| block.command.as_str())
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(block.exit_code, 0);
}

#[test]
fn shell_host_bash_stale_history_guard_still_intercepts_deduped_repeats() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    if !bash_supports_command_not_found_handler() {
        eprintln!("SKIP: bash command_not_found_handle capability is unavailable");
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-histdedup-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(home_dir.join(".bashrc"), "HISTCONTROL=ignoredups\n").expect("bashrc");

    let config = ShellHostConfig::new("bash-histdedup-test", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("please explain the last error"),
            ScriptedInput::user_line("please explain the last error"),
        ],
    )
    .expect("scripted bash pty");

    let intercepts = output
        .events
        .iter()
        .filter(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("please explain the last error")
                && event.component.as_deref() == Some("natural_language")
        })
        .count();
    assert_eq!(intercepts, 2, "{:?}", output.events);
}

// The shell-host marker layer remains observation-only: the raw input relay
// owns #2913 routing, while direct PTY submissions still prove that no global
// DEBUG trap or extdebug veto was reintroduced.
#[test]
fn shell_host_bash_missing_path_without_input_relay_fails_in_shell() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-missing-path-nl-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    // Pin a UTF-8 locale: a real terminal typing Chinese is UTF-8, while CI
    // runners default to C, where readline mangles multi-byte input bytes
    // before they ever reach the DEBUG trap (same precedent as
    // governance.rs raw-hook-test).
    let mut config = ShellHostConfig::new("missing-path-nl", &work_dir);
    config
        .env_overrides
        .push(("LANG".to_string(), "C.UTF-8".to_string()));
    config
        .env_overrides
        .push(("LC_ALL".to_string(), "C.UTF-8".to_string()));

    let prompt = "你读一下，并安装这个skill：/nonexistent-cosh-1919-probe/SKILL.md";
    let output =
        run_scripted_bash(&config, &[ScriptedInput::user_line(prompt)]).expect("scripted bash pty");

    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted && event.input.as_deref() == Some(prompt)
    }));
    let ledger = build_command_blocks(&output.events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == prompt)
        .expect("missing-path Shell block");
    assert_eq!(block.exit_code, 127);
}

/// Direct PTY input bypasses the #2913 Rust owner. The observation-only marker
/// must still redact a sensitive path-shaped failure from durable evidence.
#[test]
fn shell_host_bash_sensitive_missing_path_without_input_relay_stays_redacted() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-missing-path-sensitive-nl-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let mut config = ShellHostConfig::new("missing-path-sensitive-nl", &work_dir);
    config
        .env_overrides
        .push(("LANG".to_string(), "C.UTF-8".to_string()));
    config
        .env_overrides
        .push(("LC_ALL".to_string(), "C.UTF-8".to_string()));

    let prompt =
        "你读一下，并安装这个skill：/nonexistent-cosh-1919-probe/SKILL.md API Key: sk-fbaa6";
    let output =
        run_scripted_bash(&config, &[ScriptedInput::user_line(prompt)]).expect("scripted bash pty");

    let ledger = build_command_blocks(&output.events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == "<redacted sensitive command>")
        .expect("redacted missing-path block");
    assert_eq!(block.exit_code, 127);
    assert!(
        !format!("{:?}", output.events).contains("sk-fbaa6"),
        "{:?}",
        output.events
    );
    let journal = std::fs::read_to_string(&output.journal_path).unwrap();
    assert!(!journal.contains("sk-fbaa6"), "{journal}");
}

#[test]
fn routing_c1_cnf_han_tier_a_routes_to_agent() {
    for shell in ["bash", "zsh"] {
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c1-cnf-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let mut config = ShellHostConfig::new(format!("routing-c1-cnf-{shell}"), &work_dir);
        config.native_mode = false;
        let mut inputs = vec![
            "使用 git log --since=\"1 day ago\" --format=\"%h %s (%an, %ar)\" 总结",
            "你还好吗？ 我想问问",
        ];
        if shell == "bash" {
            inputs.push("你还好吗? 我想问问");
        }
        let steps = inputs
            .iter()
            .map(|input| ScriptedInput::user_line(*input))
            .collect::<Vec<_>>();
        let output = if shell == "bash" {
            run_scripted_bash(&config, &steps)
        } else {
            run_scripted_zsh(&config, &steps)
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        for input in inputs {
            assert!(
                output.events.iter().any(|event| {
                    event.kind == ShellEventKind::UserInputIntercepted
                        && event.input.as_deref() == Some(input)
                        && event.component.as_deref() == Some("natural_language")
                }),
                "{shell}: {input:?}: {:?}\n{}",
                output.events,
                String::from_utf8_lossy(&output.terminal_output)
            );
        }
    }
}

#[test]
fn isolated_bash_ignores_inherited_prompt_and_history_filters() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    let supports_command_not_found = bash_supports_command_not_found_handler();
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-isolated-prompt-command-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let inherited_inputrc = work_dir.join("inherited-inputrc");
    std::fs::write(
        &inherited_inputrc,
        "set input-meta off\nset convert-meta on\nset output-meta off\n",
    )
    .expect("inherited inputrc");
    let inherited_marker = work_dir.join("inherited-prompt-command-ran");
    let mut config = ShellHostConfig::new("isolated-prompt-command", &work_dir)
        .with_env(
            "PROMPT_COMMAND",
            format!("printf inherited > {}", shell_arg(&inherited_marker)),
        )
        .with_env("HISTIGNORE", "*")
        .with_env("HISTSIZE", "0")
        .with_env("HISTFILESIZE", "0")
        .with_env("INPUTRC", inherited_inputrc.display().to_string());
    config.native_mode = false;
    let input = "解释 inherited history filter";

    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(
                "printf '__cosh_hist_limits__%s:%s\\n' \"$HISTSIZE\" \"$HISTFILESIZE\"",
            ),
            ScriptedInput::user_line(input),
        ],
    )
    .expect("isolated scripted bash");

    assert!(!inherited_marker.exists());
    assert!(
        String::from_utf8_lossy(&output.terminal_output).contains("__cosh_hist_limits__1000:1000")
    );
    if supports_command_not_found {
        assert!(output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input)
                && event.component.as_deref() == Some("natural_language")
        }));
    }
}

#[test]
fn isolated_zsh_preserves_inputrc_override() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-isolated-zsh-inputrc-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let inputrc = work_dir.join("custom-inputrc");
    let mut config = ShellHostConfig::new("isolated-zsh-inputrc", &work_dir)
        .with_env("INPUTRC", inputrc.display().to_string());
    config.native_mode = false;

    let output = run_scripted_zsh(
        &config,
        &[ScriptedInput::user_line(
            "printf '__cosh_inputrc__%s\\n' \"$INPUTRC\"",
        )],
    )
    .expect("isolated scripted zsh");

    assert!(String::from_utf8_lossy(&output.terminal_output)
        .contains(&format!("__cosh_inputrc__{}", inputrc.display())));
}

#[test]
fn routing_c1_zsh_ascii_question_unmatched_routes_to_agent() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    let input = "你还好吗? 我想问问";
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-routing-c1-zsh-question-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c1-zsh-question", &work_dir);
    let output =
        run_scripted_zsh(&config, &[ScriptedInput::user_line(input)]).expect("scripted zsh");
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some(input)
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[cfg(target_os = "linux")]
#[test]
fn routing_c1_missing_path_han_tier_a_stays_shell_owned() {
    if !bash_supports_command_not_found_handler() {
        return;
    }
    let input = "打开./不存在 --dry-run \"x (preview)\"";
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-routing-c1-missing-path-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("routing-c1-missing-path", &work_dir);
    config.native_mode = false;
    let output =
        run_scripted_bash(&config, &[ScriptedInput::user_line(input)]).expect("scripted bash");
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted && event.input.as_deref() == Some(input)
    }));
    let ledger = build_command_blocks(&output.events);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == input)
        .expect("missing-path Shell block");
    assert_eq!(block.exit_code, 127);
}

#[cfg(target_os = "linux")]
#[test]
fn routing_c1_stale_history_repeated_han_prompt_routes_twice() {
    if !bash_supports_command_not_found_handler() {
        return;
    }
    let input = "解释 git log --format=\"%h (%an)\"";
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-routing-c1-stale-history-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(home_dir.join(".bashrc"), "HISTCONTROL=ignoredups\n").expect("bashrc");
    let mut config = ShellHostConfig::new("routing-c1-stale-history", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    config.native_mode = false;
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(input),
            ScriptedInput::user_line(input),
        ],
    )
    .expect("scripted bash");
    let intercepts = output
        .events
        .iter()
        .filter(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input)
                && event.component.as_deref() == Some("natural_language")
        })
        .count();
    assert_eq!(intercepts, 2, "{:?}", output.events);
}

#[test]
fn routing_c1_tier_b_side_effect_stays_native() {
    for shell in ["bash", "zsh"] {
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c1-tier-b-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&work_dir).expect("work dir");
        let side = work_dir.join("side-effect");
        let input = format!("解释 \"$(touch {})\"", shell_arg(&side));
        let config = ShellHostConfig::new(format!("routing-c1-tier-b-{shell}"), &work_dir);
        let output = if shell == "bash" {
            run_scripted_bash(&config, &[ScriptedInput::user_line(input.clone())])
        } else {
            run_scripted_zsh(&config, &[ScriptedInput::user_line(input.clone())])
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        assert!(side.exists(), "{shell}: command substitution did not run");
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input.as_str())
        }));
    }
}

#[test]
fn routing_c1_zsh_glob_qualifier_stays_native() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-routing-c1-zsh-glob-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    std::fs::write(work_dir.join("entry"), "x\n").expect("glob entry");
    let side = work_dir.join("glob-side-effect");
    let input = format!("解释 *(e:'touch {}':)", side.display());
    let config = ShellHostConfig::new("routing-c1-zsh-glob", &work_dir);
    let output = run_scripted_zsh(&config, &[ScriptedInput::user_line(input.clone())])
        .expect("scripted zsh");
    assert!(side.exists(), "glob qualifier did not run");
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some(input.as_str())
    }));
}

#[test]
fn routing_c1_valid_han_command_stays_native() {
    for shell in ["bash", "zsh"] {
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c1-valid-han-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let bin_dir = work_dir.join("bin");
        let home_dir = work_dir.join("home");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        std::fs::create_dir_all(&home_dir).expect("home dir");
        let executable = bin_dir.join("解释");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '__han_exec__:%s\\n' \"$1\"\n",
        )
        .expect("han executable");
        make_executable(&executable);
        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let rc = "解释函数() { printf '__han_function__:%s\\n' \"$1\"; }\n\
                  alias 解释别名=\"printf '__han_alias__:%s\\n'\"\n";
        std::fs::write(
            home_dir.join(if shell == "bash" { ".bashrc" } else { ".zshrc" }),
            rc,
        )
        .expect("shell rc");
        let mut config = ShellHostConfig::new(format!("routing-c1-valid-han-{shell}"), &work_dir)
            .with_env("PATH", path)
            .with_env("HOME", home_dir.display().to_string());
        if shell == "zsh" {
            config = config.with_env("COSH_ZDOTDIR_ORIG", home_dir.display().to_string());
        }
        let inputs = ["解释 ok", "解释函数 ok", "解释别名 ok"];
        let steps = inputs
            .iter()
            .map(|input| ScriptedInput::user_line(*input))
            .collect::<Vec<_>>();
        let output = if shell == "bash" {
            run_scripted_bash(&config, &steps)
        } else {
            run_scripted_zsh(&config, &steps)
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        let terminal = String::from_utf8_lossy(&output.terminal_output);
        for marker in ["__han_exec__:ok", "__han_function__:ok", "__han_alias__:ok"] {
            assert!(terminal.contains(marker), "{shell}: {marker}: {terminal}");
        }
        for input in inputs {
            assert!(!output.events.iter().any(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some(input)
            }));
        }
    }
}

fn assert_routing_c2_quote_cnf(shell: &str) {
    if shell == "bash" && !bash_supports_command_not_found_handler() {
        return;
    }
    if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    let inputs = ["'子曰 三人行'后续 请解释", "\"子 曰\"后续 请解释"];
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-routing-c2-quote-{shell}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new(format!("routing-c2-quote-{shell}"), &work_dir);
    let steps = inputs
        .iter()
        .map(|input| ScriptedInput::user_line(*input))
        .collect::<Vec<_>>();
    let output = if shell == "bash" {
        run_scripted_bash(&config, &steps)
    } else {
        run_scripted_zsh(&config, &steps)
    }
    .unwrap_or_else(|error| panic!("{shell}: {error}"));
    for input in inputs {
        assert!(
            output.events.iter().any(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some(input)
                    && event.component.as_deref() == Some("natural_language")
            }),
            "{shell}: {input:?}: {:?}\n{}",
            output.events,
            String::from_utf8_lossy(&output.terminal_output)
        );
    }
}

#[test]
fn routing_c2_bash_quote_cnf_routes_to_agent() {
    assert_routing_c2_quote_cnf("bash");
}

#[test]
fn routing_c2_zsh_quote_cnf_routes_to_agent() {
    assert_routing_c2_quote_cnf("zsh");
}

#[test]
fn routing_c2_expansion_drift_and_matched_arguments_stay_native() {
    for shell in ["bash", "zsh"] {
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c2-drift-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let home_dir = work_dir.join("home");
        std::fs::create_dir_all(&home_dir).expect("home dir");
        std::fs::write(work_dir.join("match.log"), "x\n").expect("glob match");
        let side = work_dir.join("alias-side");
        let rc = format!("alias 解释='解释; touch {}'\n", side.display());
        std::fs::write(
            home_dir.join(if shell == "bash" { ".bashrc" } else { ".zshrc" }),
            rc,
        )
        .expect("shell rc");
        let mut config = ShellHostConfig::new(format!("routing-c2-drift-{shell}"), &work_dir)
            .with_env("HOME", home_dir.display().to_string());
        if shell == "zsh" {
            config = config.with_env("COSH_ZDOTDIR_ORIG", home_dir.display().to_string());
        }
        let alias_input = "解释";
        let glob_input = "说明 *.log";
        let cd_input = format!("cd {}", shell_arg(&work_dir));
        let output = if shell == "bash" {
            run_scripted_bash(
                &config,
                &[
                    ScriptedInput::user_line(cd_input.clone()),
                    ScriptedInput::user_line(alias_input),
                    ScriptedInput::user_line(glob_input),
                ],
            )
        } else {
            run_scripted_zsh(
                &config,
                &[
                    ScriptedInput::user_line(cd_input),
                    ScriptedInput::user_line(alias_input),
                    ScriptedInput::user_line(glob_input),
                ],
            )
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        assert!(side.exists(), "{shell}: alias side effect missing");
        for input in [alias_input, glob_input] {
            assert!(
                !output.events.iter().any(|event| {
                    event.kind == ShellEventKind::UserInputIntercepted
                        && event.input.as_deref() == Some(input)
                }),
                "{shell}: {input}: {:?}",
                output.events
            );
        }
    }
}

#[test]
fn routing_c2_long_mixed_language_prompts_route_to_agent() {
    // Issue #2053: zsh preexec abbreviates its second argument for long
    // inputs; comparing the canonicalized command against it produced a
    // false expansion-drift signal that pushed long CJK/ASCII prompts to
    // the native command-not-found path instead of the Agent. Cover both
    // sides of the abbreviated-text boundary plus a Chinese/English space
    // variation, and keep bash as the consistency control.
    let inputs = [
        // Above the abbreviation boundary (the original user report).
        "刚才没有恢复，不过 macOS 显示管理器认为屏幕在线",
        // Space removed at the Chinese/English boundary.
        "刚才没有恢复，不过macOS 显示管理器认为屏幕在线",
        // Below the abbreviation boundary.
        "刚才没有恢复 请解释",
    ];
    for shell in ["bash", "zsh"] {
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c2-long-nl-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let config = ShellHostConfig::new(format!("routing-c2-long-nl-{shell}"), &work_dir);
        let steps = inputs
            .iter()
            .map(|input| ScriptedInput::user_line(*input))
            .collect::<Vec<_>>();
        let output = if shell == "bash" {
            run_scripted_bash(&config, &steps)
        } else {
            run_scripted_zsh(&config, &steps)
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        let terminal = String::from_utf8_lossy(&output.terminal_output);
        for input in inputs {
            let first_word = input.split_whitespace().next().expect("first word");
            assert!(
                !terminal.contains(&format!("command not found: {first_word}")),
                "{shell}: {input:?} hit native command-not-found: {terminal}"
            );
            assert!(
                output.events.iter().any(|event| {
                    event.kind == ShellEventKind::UserInputIntercepted
                        && event.input.as_deref() == Some(input)
                        && event.component.as_deref() == Some("natural_language")
                }),
                "{shell}: {input:?}: {:?}\n{terminal}",
                output.events
            );
            assert!(
                !output.events.iter().any(|event| {
                    event.kind == ShellEventKind::CommandFailed
                        && event.command.as_deref() == Some(input)
                        && event.exit_code == Some(127)
                }),
                "{shell}: {input:?} failed with exit 127: {:?}\n{terminal}",
                output.events
            );
        }
    }
}

#[test]
fn routing_c2_nested_provenance_marks_only_the_outer_missing_command() {
    let input = "解释 \"$(解释)\"";
    for shell in ["bash", "zsh"] {
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c2-nested-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let config = ShellHostConfig::new(format!("routing-c2-nested-{shell}"), &work_dir);
        let output = if shell == "bash" {
            run_scripted_bash(&config, &[ScriptedInput::user_line(input)])
        } else {
            run_scripted_zsh(&config, &[ScriptedInput::user_line(input)])
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        let proven = output
            .events
            .iter()
            .filter(|event| {
                event
                    .routing
                    .as_ref()
                    .is_some_and(|routing| routing.top_level_missing && routing.proven)
            })
            .count();
        assert_eq!(proven, 1, "{shell}: {:?}", output.events);
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input)
        }));
    }
}

#[test]
fn routing_c2_nested_provenance_rejects_same_token_from_shell_function() {
    for shell in ["bash", "zsh"] {
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c2-function-nested-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let home_dir = work_dir.join("home");
        std::fs::create_dir_all(&home_dir).expect("home dir");
        let rc = if shell == "bash" {
            "解释() { unset -f 解释; 解释; }\n"
        } else {
            "解释() { unfunction 解释; 解释; }\n"
        };
        std::fs::write(
            home_dir.join(if shell == "bash" { ".bashrc" } else { ".zshrc" }),
            rc,
        )
        .expect("shell rc");
        let mut config =
            ShellHostConfig::new(format!("routing-c2-function-nested-{shell}"), &work_dir)
                .with_env("HOME", home_dir.display().to_string());
        if shell == "zsh" {
            config = config.with_env("COSH_ZDOTDIR_ORIG", home_dir.display().to_string());
        }
        let output = if shell == "bash" {
            run_scripted_bash(&config, &[ScriptedInput::user_line("解释")])
        } else {
            run_scripted_zsh(&config, &[ScriptedInput::user_line("解释")])
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        assert!(
            !output.events.iter().any(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    || event
                        .routing
                        .as_ref()
                        .is_some_and(|routing| routing.top_level_missing && routing.proven)
            }),
            "{shell}: {:?}",
            output.events
        );
    }
}

#[test]
fn routing_c2_delegate_unsupported_escape_and_matched_glob() {
    for shell in ["bash", "zsh"] {
        if shell == "bash" && !bash_supports_command_not_found_handler() {
            continue;
        }
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c2-unsupported-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&work_dir).expect("work dir");
        std::fs::write(work_dir.join("中文a"), "x\n").expect("glob match");
        let inputs = [r"解释\ 一下", "中文?"];
        let config = ShellHostConfig::new(format!("routing-c2-unsupported-{shell}"), &work_dir);
        let mut steps = vec![ScriptedInput::user_line(format!(
            "cd {}",
            shell_arg(&work_dir)
        ))];
        steps.extend(inputs.iter().map(|input| ScriptedInput::user_line(*input)));
        let output = if shell == "bash" {
            run_scripted_bash(&config, &steps)
        } else {
            run_scripted_zsh(&config, &steps)
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        for input in inputs {
            assert!(
                !output.events.iter().any(|event| {
                    event.kind == ShellEventKind::UserInputIntercepted
                        && event.input.as_deref() == Some(input)
                }),
                "{shell}: {input}: {:?}",
                output.events
            );
        }
    }
}

#[test]
fn routing_c2_valid_quoted_command_and_inner_whitespace_keep_their_owners() {
    for shell in ["bash", "zsh"] {
        if shell == "zsh" && Command::new("zsh").arg("--version").output().is_err() {
            continue;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-routing-c2-valid-quoted-{shell}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let bin_dir = work_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        let executable = bin_dir.join("中文 命令");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '__quoted_exec__:%s\\n' \"$1\"\n",
        )
        .expect("quoted executable");
        make_executable(&executable);
        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let config = ShellHostConfig::new(format!("routing-c2-valid-quoted-{shell}"), &work_dir)
            .with_env("PATH", path);
        let native = "\"中文 命令\" ok";
        let natural = "解释  一下";
        let output = if shell == "bash" {
            run_scripted_bash(
                &config,
                &[
                    ScriptedInput::user_line(native),
                    ScriptedInput::user_line(natural),
                ],
            )
        } else {
            run_scripted_zsh(
                &config,
                &[
                    ScriptedInput::user_line(native),
                    ScriptedInput::user_line(natural),
                ],
            )
        }
        .unwrap_or_else(|error| panic!("{shell}: {error}"));
        assert!(
            String::from_utf8_lossy(&output.terminal_output).contains("__quoted_exec__:ok"),
            "{shell}: {}",
            String::from_utf8_lossy(&output.terminal_output)
        );
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(native)
        }));
        if shell == "zsh" || bash_supports_command_not_found_handler() {
            assert!(
                output.events.iter().any(|event| {
                    event.kind == ShellEventKind::UserInputIntercepted
                        && event.input.as_deref() == Some(natural)
                }),
                "{shell}: {:?}",
                output.events
            );
        }
    }
}

// The observation-only marker keeps existing paths and plain-English typo
// paths byte-identical when submissions bypass the #2913 raw-input owner.
#[test]
fn shell_host_bash_missing_path_counterproofs_stay_native() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-missing-path-native-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    // I1: existing executable script keeps running natively.
    let probe_path = work_dir.join("probe-1919.sh");
    let executed_path = work_dir.join("probe-1919-executed");
    std::fs::write(
        &probe_path,
        format!("#!/bin/sh\ntouch {}\n", executed_path.display()),
    )
    .expect("probe script");
    make_executable(&probe_path);
    // D6: existing but non-executable file as first word stays native.
    let data_path = work_dir.join("data-1919.txt");
    std::fs::write(&data_path, "plain data\n").expect("data file");
    // Review counterproofs: dangling symlink and permission-opaque paths
    // are not provably ENOENT, so the missing-path branch must stand down
    // and leave the native 126/127 outcome untouched.
    let dangling_path = work_dir.join("dangling-1919");
    std::os::unix::fs::symlink(work_dir.join("no-such-target"), &dangling_path)
        .expect("dangling symlink");
    let opaque_dir = work_dir.join("opaque-1919");
    std::fs::create_dir_all(&opaque_dir).expect("opaque dir");
    let opaque_file = opaque_dir.join("real-file");
    std::fs::write(&opaque_file, "x\n").expect("opaque file");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&opaque_dir, std::fs::Permissions::from_mode(0o000))
            .expect("chmod opaque");
    }

    // UTF-8 locale for the same readline multi-byte reason as the positive
    // intercept case above.
    let mut config = ShellHostConfig::new("missing-path-native", &work_dir);
    config
        .env_overrides
        .push(("LANG".to_string(), "C.UTF-8".to_string()));
    config
        .env_overrides
        .push(("LC_ALL".to_string(), "C.UTF-8".to_string()));
    let existing_exec = probe_path.display().to_string();
    let existing_data = format!("{} 帮我读一下", data_path.display());
    let dangling_input = format!("{} 帮我读一下", dangling_path.display());
    let opaque_input = format!("{} 帮我读一下", opaque_file.display());
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line(existing_exec.clone()),
            ScriptedInput::user_line("/usr/bin/nonexistent-cosh-1919-probe"),
            ScriptedInput::user_line(existing_data.clone()),
            ScriptedInput::user_line(dangling_input.clone()),
            ScriptedInput::user_line(opaque_input.clone()),
        ],
    )
    .expect("scripted bash pty");

    // I1: the existing script actually executed.
    assert!(
        executed_path.exists(),
        "existing script must run natively: {:?}",
        output.events
    );
    // No missing-path input may surface as a natural-language intercept.
    assert!(
        !output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.component.as_deref() == Some("natural_language")
        }),
        "{:?}",
        output.events
    );
    // I2: the plain-English typo path keeps the native bash error.
    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("No such file or directory"), "{terminal}");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&opaque_dir, std::fs::Permissions::from_mode(0o755))
            .expect("restore opaque");
    }
}
