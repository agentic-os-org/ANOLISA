use super::*;

#[cfg(target_os = "linux")]
pub(super) fn details_page_position(
    screen: &vt100::Screen,
    language: &str,
) -> Option<(usize, usize)> {
    screen.contents().lines().find_map(|line| {
        let line = line.trim().trim_matches('│').trim();
        let position = line.strip_prefix(if language == "zh" { "第 " } else { "Page " })?;
        let (page, pages) = position.trim_end_matches(" 页").split_once('/')?;
        Some((page.parse().ok()?, pages.parse().ok()?))
    })
}

#[cfg(target_os = "linux")]
pub(super) fn page_through_details(
    session: &mut crate::support::terminal_screen::TerminalSession,
    language: &str,
    turn_consent: bool,
) {
    let (first_page, pages) =
        details_page_position(session.screen(), language).expect("visible Details page position");
    assert_eq!(first_page, 1);
    assert!(pages > 1, "40x16 Details must exercise paging");
    let mut contents = Vec::new();
    for page in 1..=pages {
        let screen = session.screen().contents();
        assert!(
            screen.contains(if language == "zh" { "拒绝" } else { "Deny" }),
            "{screen}"
        );
        assert!(
            screen.contains(if language == "zh" {
                "> [ 详情 ]"
            } else {
                "> [ Details ]"
            }),
            "{screen}"
        );
        let rows = screen
            .lines()
            .skip_while(|line| {
                !line.contains(if language == "zh" {
                    "审批详情 "
                } else {
                    "Approval details "
                })
            })
            .skip(1)
            .take_while(|line| {
                !line
                    .trim()
                    .trim_matches('│')
                    .trim()
                    .starts_with(if language == "zh" { "第 " } else { "Page " })
            })
            .map(|line| line.trim().trim_matches('│').trim().to_string())
            .collect::<Vec<_>>();
        assert!(!rows.is_empty(), "{screen}");
        contents.push(rows);
        if page < pages {
            session.send(b"\x1b[B");
            session.wait_screen("40x16 next Details page", |screen| {
                details_page_position(screen, language) == Some((page + 1, pages))
            });
        }
    }
    let common_header = (0..contents.iter().map(Vec::len).min().unwrap())
        .take_while(|index| {
            contents
                .iter()
                .all(|rows| rows[*index] == contents[0][*index])
        })
        .count();
    let body: String = contents
        .iter()
        .flat_map(|rows| rows.iter().skip(common_header))
        .flat_map(|row| row.chars().filter(|character| !character.is_whitespace()))
        .collect();
    for phrase in if language == "zh" {
        [
            "实时终端输出原样显示",
            "可能留在滚屏",
            "不会改变终端输出",
            "preview_redacted",
            "截断或家目录替换",
            "preview_included不证明没有凭据",
        ]
    } else {
        [
            "Liveterminaloutputisshownas-is",
            "maystayinscrollback",
            "doesnotredacttheterminal",
            "preview_redacted",
            "truncationorhome-pathreplacement",
            "preview_includeddoesnotprovesecretsareabsent",
        ]
    } {
        assert!(body.contains(phrase), "missing {phrase}: {body}");
    }
    if turn_consent {
        for phrase in if language == "zh" {
            [
                "会批准当前项",
                "同轮已排队和后来到达",
                "高风险",
                "强制Hook",
                "本地Shell",
                "缺失执行绑定",
                "被挡重试",
            ]
        } else {
            [
                "approvesthecurrentrequest",
                "eligiblequeuedandlater",
                "High-riskrequests",
                "forcedHooks",
                "localshellrequests",
                "missingexecutionbindings",
                "blockedretries",
            ]
        } {
            assert!(body.contains(phrase), "missing {phrase}: {body}");
        }
    }
    session.send(b"\x1b[A");
    session.wait_screen("40x16 previous Details page", |screen| {
        details_page_position(screen, language) == Some((pages - 1, pages))
    });
    session.send(b"\n");
    session.wait_screen("40x16 paged Details Enter remains pending", |screen| {
        details_page_position(screen, language) == Some((pages - 1, pages))
            && screen.contents().contains(if language == "zh" {
                "> [ 详情 ]"
            } else {
                "> [ Details ]"
            })
    });
}

#[test]
#[cfg(target_os = "linux")]
fn raw_cli_output_disclosure_real_40x16_warning_focus_and_denial() {
    use crate::support::terminal_screen::TerminalSession;

    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[(
                    ".copilot-shell/config.toml",
                    "[shell]\nadapter_default = 'fake'\n",
                )],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("COSH_SHELL_WIDTH", "40"),
                    (
                        "TEST_SYNTHETIC_VALUE",
                        "SYNTHETIC_APPROVAL_OUTPUT_MUST_NOT_RUN",
                    ),
                ],
                &format!("environment-deny-{shell}-{language}-40x16"),
            );
            let config = session.home().join(".copilot-shell/config.toml");
            let original_config = fs::read(&config).unwrap();
            session.resize_to(16, 40);
            session.send(b"?? approval output fixture: printenv TEST_SYNTHETIC_VALUE\n");
            session.wait_screen("40x16 output warning and denial", |screen| {
                let text = screen.contents();
                text.contains(if language == "zh" {
                    "环境变量"
                } else {
                    "Environment"
                }) && text.contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            session.send(b"d");
            session.wait_screen("40x16 details focus and boundary", |screen| {
                let text: String = screen
                    .contents()
                    .chars()
                    .filter(|character| !character.is_whitespace() && *character != '│')
                    .collect();
                text.contains(if language == "zh" {
                    "上下翻页"
                } else {
                    "Up/Downpage"
                }) && text.contains(if language == "zh" {
                    ">[详情]"
                } else {
                    ">[Details]"
                }) && text.contains(if language == "zh" {
                    "审批详情req-1"
                } else {
                    "Approvaldetailsreq-1"
                })
            });
            session.send(b"\n");
            session.wait_screen("40x16 details Enter remains pending", |screen| {
                screen.contents().contains(if language == "zh" {
                    "> [ 详情 ]"
                } else {
                    "> [ Details ]"
                })
            });
            page_through_details(&mut session, language, false);
            let denial_is_focused = |screen: &vt100::Screen| {
                let text = screen.contents();
                text.contains(if language == "zh" {
                    "> [ 拒绝 ]"
                } else {
                    "> [ Deny ]"
                }) && text.contains(if language == "zh" {
                    "环境变量"
                } else {
                    "Environment"
                }) && details_page_position(screen, language).is_none()
            };
            session.send(b"\x1b[D");
            session.wait_screen("40x16 Left returns to denial focus", denial_is_focused);
            session.send(b"d");
            session.wait_screen("40x16 Details reopened", |screen| {
                details_page_position(screen, language).is_some()
            });
            session.send(b"\x1b[C");
            session.wait_screen("40x16 Right returns to denial focus", denial_is_focused);
            session.send(b"\n");
            session.wait_screen("40x16 denied receipt", |screen| {
                screen.contents().contains(if language == "zh" {
                    "已拒绝"
                } else {
                    "Denied req-1"
                })
            });
            assert!(!session
                .raw_output()
                .contains("SYNTHETIC_APPROVAL_OUTPUT_MUST_NOT_RUN"));
            assert_eq!(fs::read(&config).unwrap(), original_config);
            session.finish();
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn raw_cli_output_disclosure_batch_details_real_40x16() {
    use crate::support::terminal_screen::TerminalSession;

    for language in ["en", "zh"] {
        let mut session = TerminalSession::spawn_for_shell_with_recording(
            "bash",
            "enhanced",
            40,
            &[],
            &[
                ("COSH_SHELL_LANG", language),
                ("COSH_SHELL_WIDTH", "40"),
                (
                    "TEST_SYNTHETIC_VALUE",
                    "SYNTHETIC_APPROVAL_OUTPUT_MUST_NOT_RUN",
                ),
            ],
            &format!("combined-batch-bash-{language}-40x16"),
        );
        let config = session.home().join(".copilot-shell/config.toml");
        let original_config = fs::read(&config).unwrap();
        session.resize_to(16, 40);
        session.send(b"?? approval output batch fixture: uptime; printenv; ps aux; cat ~/.ssh/id_rsa ||| custom-inspect\n");
        session.wait_screen("40x16 combined batch output disclosure", |screen| {
            let text: String = screen
                .contents()
                .chars()
                .filter(|character| !character.is_whitespace() && *character != '│')
                .collect();
            let phrases = if language == "zh" {
                [
                    "审批req-1",
                    "环境变量",
                    "敏感内容",
                    "完整进程参数",
                    "本轮全部允许",
                    "拒绝",
                ]
            } else {
                [
                    "Approvalreq-1",
                    "Envvalues",
                    "sensitivedata",
                    "fullargs",
                    "Allowallthisturn",
                    "Deny",
                ]
            };
            phrases.iter().all(|phrase| text.contains(phrase))
        });
        session.send(b"d");
        session.wait_screen("40x16 combined batch Details focus", |screen| {
            let text: String = screen
                .contents()
                .chars()
                .filter(|character| !character.is_whitespace() && *character != '│')
                .collect();
            let phrases = if language == "zh" {
                [
                    "审批详情req-1",
                    "环境变量",
                    "敏感内容",
                    "完整进程参数",
                    ">[详情]",
                    "拒绝",
                ]
            } else {
                [
                    "Approvaldetailsreq-1",
                    "Envvalues",
                    "sensitivedata",
                    "fullargs",
                    ">[Details]",
                    "Deny",
                ]
            };
            phrases.iter().all(|phrase| text.contains(phrase))
        });
        session.send(b"\n");
        session.wait_screen("40x16 combined batch remains pending", |screen| {
            screen.contents().contains(if language == "zh" {
                "> [ 详情 ]"
            } else {
                "> [ Details ]"
            })
        });
        page_through_details(&mut session, language, true);
        assert!(!session.raw_output().contains("Denied req-1"));
        assert!(!session.raw_output().contains("Approved req-1"));
        session.send(b"\x1b[D");
        session.wait_screen("40x16 batch return to denial", |screen| {
            screen.contents().contains(if language == "zh" {
                "> [ 拒绝 ]"
            } else {
                "> [ Deny ]"
            })
        });
        session.send(b"\x1b");
        session.wait_screen("40x16 batch cancellation", |screen| {
            screen.contents().contains(if language == "zh" {
                "审批 req-2"
            } else {
                "Approval req-2"
            })
        });
        let output = strip_ansi_escape(&session.raw_output());
        assert!(output.contains(if language == "zh" {
            "已取消 req-1"
        } else {
            "Cancelled req-1"
        }));
        session.send(b"\x1b");
        session.wait_screen("40x16 remaining batch request cancellation", |screen| {
            screen.contents().contains(if language == "zh" {
                "已取消 req-2"
            } else {
                "Cancelled req-2"
            })
        });
        assert!(!session
            .raw_output()
            .contains("SYNTHETIC_APPROVAL_OUTPUT_MUST_NOT_RUN"));
        assert_eq!(fs::read(&config).unwrap(), original_config);
        session.finish();
    }
}

#[test]
#[cfg(target_os = "linux")]
fn raw_cli_details_shortcut_enter_keeps_real_pty_request_pending() {
    use crate::support::terminal_screen::TerminalSession;

    for (request, turn_consent) in [
        ("?? stream pwd tool approval\n", false),
        ("?? stream batch tool approval\n", true),
        ("?? stream tool approval\n", false),
    ] {
        let mut session = TerminalSession::spawn_for_shell_with_startup_env(
            "bash",
            "enhanced",
            80,
            &[
                (
                    ".copilot-shell/config.toml",
                    "[shell]\nadapter_default = 'fake'\nreadonly_disabled = ['pwd']\n",
                ),
                ("bin/git", "#!/bin/sh\nexit 0\n"),
                (
                    "bin/ps",
                    "#!/bin/sh\nprintf executed > \"$HOME/unexpected-execution\"\n",
                ),
            ],
            &[("PATH", "$HOME/bin:/usr/bin:/bin")],
        );
        let config = session.home().join(".copilot-shell/config.toml");
        let original_config = fs::read(&config).unwrap();
        session.send(request.as_bytes());
        session.wait_screen("approval request", |screen| {
            let contents = screen.contents();
            contents.contains("Approval req-1")
                && contents.contains("Details")
                && (!turn_consent || contents.contains("Allow all this turn"))
        });
        session.send(b"d");
        session.wait_screen("details shortcut focus", |screen| {
            screen.contents().contains("> [ Details ]")
        });
        session.send(b"\n");
        session.wait_screen("details Enter remains pending", |screen| {
            let contents = screen.contents();
            contents.contains("Approval details req-1") && contents.contains("> [ Details ]")
        });
        let output = session.raw_output();
        assert!(!output.contains("Denied req-1"), "{output}");
        assert!(!output.contains("Approved req-1"), "{output}");
        assert!(!output.contains("Trusted req-1"), "{output}");
        assert!(!session.home().join("unexpected-execution").exists());
        assert_eq!(fs::read(&config).unwrap(), original_config);
        session.send(b"\x1b");
        session.wait_screen("details Escape cancellation", |screen| {
            screen.contents().contains("Cancelled req-1")
        });
    }
}

#[test]
fn raw_cli_details_approvals_renders_decision_journal_panel() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? request tool approval\n".to_vec(), Duration::ZERO),
        (b"\n".to_vec(), Duration::from_millis(2_500)),
        (
            b"/details approvals\n".to_vec(),
            Duration::from_millis(1_500),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(500)),
    ]);

    assert!(output.contains("Approval journal"), "{output}");
    assert!(output.contains("1 decisions"), "{output}");
    assert!(
        output.contains("req-1") && output.contains("approved"),
        "{output}"
    );
    assert!(
        !(output.contains("req-2") && output.contains("denied")),
        "{output}"
    );
    assert!(output.contains("Command: $ git status"), "{output}");
    assert!(
        output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(output.contains("Command block:"), "{output}");
    assert!(output.contains("Redaction: ref_only"), "{output}");
    assert!(
        !output.contains("touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{output}"
    );
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_details_approvals_records_denied_not_executed() {
    let output = run_raw_cli_ask_with_args_and_marker_input(
        &[],
        &[
            ("cosh-osc$", b"?? stream tool approval\n"),
            ("Approval req-1", b"\x1b[C\n"),
            (
                "Command was not executed for req-1",
                b"/details approvals\n",
            ),
            ("Approval journal", b"exit\n"),
        ],
    );

    assert!(output.contains("Denied req-1"), "{output}");
    assert!(output.contains("Approval journal"), "{output}");
    assert!(output.contains("1 decisions"), "{output}");
    assert!(
        output.contains("req-1") && output.contains("denied"),
        "{output}"
    );
    assert!(
        output.contains("Execution: not_executed_denied"),
        "{output}"
    );
    assert!(
        !output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(output.contains("Command block: <none>"), "{output}");
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_details_approvals_records_cancelled_not_executed() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"\x1b\n".to_vec(), Duration::from_millis(2_500)),
        (
            b"/details approvals\n".to_vec(),
            Duration::from_millis(1_000),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(500)),
    ]);

    assert!(output.contains("Cancelled req-1"), "{output}");
    assert!(output.contains("Approval journal"), "{output}");
    assert!(output.contains("1 decisions"), "{output}");
    assert!(
        output.contains("req-1") && output.contains("cancelled"),
        "{output}"
    );
    assert!(
        output.contains("Execution: not_executed_cancelled"),
        "{output}"
    );
    assert!(
        !output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(output.contains("Command block: <none>"), "{output}");
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_details_approvals_uses_zh_language_env() {
    let output = run_raw_cli_ask_with_args_env_and_delayed_input(
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"?? request tool approval\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(2_500)),
            (
                b"/details approvals\n".to_vec(),
                Duration::from_millis(1_500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(output.contains("审批记录"), "{output}");
    assert!(output.contains("1 条决策"), "{output}");
    assert!(
        output.contains("req-1") && output.contains("approved"),
        "{output}"
    );
    assert!(output.contains("命令: $ git status"), "{output}");
    assert!(output.contains("执行: foreground_shell_pty"), "{output}");
    assert!(output.contains("命令块:"), "{output}");
    assert!(output.contains("脱敏: ref_only"), "{output}");
    assert!(!output.contains("Approval journal"), "{output}");
    assert!(!output.contains("Command: $ git status"), "{output}");
    assert!(
        !output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(!output.contains("Command block:"), "{output}");
    assert!(!output.contains("Redaction: ref_only"), "{output}");
    assert!(
        !output.contains("touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{output}"
    );
    assert!(!output.contains("bash: /details"), "{output}");
    assert_no_migrated_english_ui_labels(&output, APPROVAL_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_details_for_approval_uses_structured_panel() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"d".to_vec(), Duration::from_millis(2_500)),
        (b"\x1b".to_vec(), Duration::from_millis(300)),
        (b"exit\n".to_vec(), Duration::from_millis(800)),
    ]);

    assert_approval_prompt_visible(&output);
    assert!(output.contains("tool request"), "{output}");
    assert!(output.contains("Cancelled req-1"), "{output}");
    assert!(output.contains("high risk"), "{output}");
    assert!(output.contains("may run repo-embedded scripts"), "{output}");
    let compact: String = strip_ansi_escape(&output)
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '│')
        .collect();
    assert!(compact.contains("Up/Downpage"), "{output}");
    assert!(!output.contains("Keys:"), "{output}");
    assert!(output.contains("Command:"), "{output}");
    assert!(output.contains("git status"), "{output}");
    assert!(!output.contains("Subject: tool shell"), "{output}");
    assert!(!output.contains("id: req-1"), "{output}");
    assert!(!output.contains("preview: git status"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_details_for_approval_uses_zh_language_env() {
    let output = run_raw_cli_ask_with_args_env_and_delayed_input(
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
            (b"d".to_vec(), Duration::from_millis(2_500)),
            (b"\x1b".to_vec(), Duration::from_millis(300)),
            (b"exit\n".to_vec(), Duration::from_millis(800)),
        ],
    );

    assert_zh_approval_prompt_visible(&output);
    assert!(output.contains("已取消 req-1"), "{output}");
    assert!(output.contains("Bash · 高风险"), "{output}");
    assert!(output.contains("可能执行仓库内嵌脚本"), "{output}");
    assert_inline_before_followup(&output, "输出无法预判", "已取消 req-1");
    assert!(!output.contains("风险 medium"), "{output}");
    assert!(output.contains("上下翻页"), "{output}");
    assert!(output.contains("命令:"), "{output}");
    assert!(output.contains("git status"), "{output}");
    assert!(!output.contains("Approval required"), "{output}");
    assert!(!output.contains("Approval req-1"), "{output}");
    assert!(!output.contains("Approval details"), "{output}");
    assert!(!output.contains("Cancelled req-1"), "{output}");
    assert!(
        !output.contains("This tool request requires your approval."),
        "{output}"
    );
    assert!(!output.contains("Keys:"), "{output}");
    assert!(!output.contains("Command:"), "{output}");
    assert!(!output.contains("Subject: tool shell"), "{output}");
    assert!(!output.contains("id: req-1"), "{output}");
    assert!(!output.contains("preview: git status"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
    assert_no_migrated_english_ui_labels(&output, APPROVAL_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_multiline_bash_tool_is_visible_in_approval_details() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (
            b"?? stream multiline tool approval\n".to_vec(),
            Duration::ZERO,
        ),
        (b"d".to_vec(), Duration::from_millis(2_500)),
        (b"\x1b".to_vec(), Duration::from_millis(300)),
        (b"exit\n".to_vec(), Duration::from_millis(200)),
    ]);

    assert_approval_prompt_visible(&output);
    assert!(output.contains("Command:"), "{output}");
    assert!(output.contains("printf one"), "{output}");
    assert!(output.contains("printf two"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}
