use super::*;
use crate::support::terminal_screen::TerminalSession;

fn visible_text(screen: &vt100::Screen) -> String {
    screen
        .contents()
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '│')
        .collect()
}

#[test]
fn raw_cli_approval_recording_ordinary_metrics_real_40x16() {
    record_output_source("ordinary-metrics", "uptime", "", "");
}

#[test]
fn raw_cli_approval_recording_sensitive_source_real_40x16() {
    record_output_source(
        "sensitive-source",
        "cat ~/.ssh/id_rsa",
        "Sensitivecontent",
        "敏感内容",
    );
}

#[test]
fn raw_cli_approval_recording_process_arguments_real_40x16() {
    record_output_source(
        "process-arguments",
        "ps -eo args=CMD",
        "Fullprocessarguments",
        "完整进程参数",
    );
}

#[test]
fn raw_cli_approval_recording_attached_process_arguments_real_40x16() {
    record_output_source(
        "attached-process-arguments",
        "ps -eoargs=CMD",
        "Fullprocessarguments",
        "完整进程参数",
    );
}

#[test]
fn raw_cli_approval_recording_process_arguments_width_real_40x16() {
    record_output_source(
        "process-arguments-width",
        "ps -o args:80",
        "Fullprocessarguments",
        "完整进程参数",
    );
}

#[test]
fn raw_cli_approval_recording_ao_process_arguments_real_40x16() {
    record_output_source(
        "process-arguments-ao",
        "ps -Ao pid,command",
        "Fullprocessarguments",
        "完整进程参数",
    );
    record_output_source(
        "process-arguments-ao-attached",
        "ps -Aopid,command",
        "Fullprocessarguments",
        "完整进程参数",
    );
}

#[test]
fn raw_cli_approval_recording_environment_option_terminator_real_40x16() {
    record_output_source(
        "environment-option-terminator",
        "printenv -- --help",
        "Environment",
        "环境变量",
    );
}

#[test]
fn raw_cli_approval_recording_environment_attached_unset_real_40x16() {
    record_output_source(
        "environment-attached-unset",
        "env -uHOME",
        "Environment",
        "环境变量",
    );
}

#[test]
fn raw_cli_approval_recording_process_name_real_40x16() {
    record_output_source("process-name", "ps -eo pid,comm", "", "");
}

#[test]
fn raw_cli_approval_recording_unknown_output_real_40x16() {
    record_output_source(
        "unknown-output",
        "synthetic-inspect",
        "Unpredictable",
        "无法预判",
    );
}

#[test]
fn raw_cli_approval_recording_wrapper_output_real_40x16() {
    record_output_source(
        "wrapper-output",
        "time -v uptime",
        "Unpredictable",
        "无法预判",
    );
}

#[test]
fn raw_cli_approval_recording_sudo_login_output_real_40x16() {
    record_output_source(
        "sudo-login-output",
        "sudo -i uptime",
        "Unpredictable",
        "无法预判",
    );
}

#[test]
fn raw_cli_approval_recording_ordinary_process_metrics_real_40x16() {
    record_output_source(
        "ordinary-process-metrics",
        "ps -eo pid,ppid,user,%cpu,%mem,rss,etime,comm",
        "",
        "",
    );
}

#[test]
fn raw_cli_approval_recording_allow_real_40x16() {
    record_decision("allow", b"\n", "Approved req-1", "已批准 req-1");
}

#[test]
fn raw_cli_approval_recording_deny_real_40x16() {
    record_decision("deny", b"\x1b[C\n", "Denied req-1", "已拒绝 req-1");
}

#[test]
fn raw_cli_approval_recording_cancel_real_40x16() {
    record_decision("cancel", b"\x1b", "Cancelled req-1", "已取消 req-1");
}

#[test]
fn raw_cli_approval_recording_irrecoverable_denial_real_40x16() {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[(
                    "bin/reboot",
                    "#!/bin/sh\nprintf unexpected > \"$HOME/reboot-attempt\"\n",
                )],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("COSH_SHELL_WIDTH", "40"),
                    ("PATH", "$HOME/bin:/usr/bin:/bin"),
                ],
                &format!("irrecoverable-deny-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(b"?? approval output fixture: reboot\n");
            session.wait_screen("irrecoverable warning and denial", |screen| {
                let text = visible_text(screen);
                let phrases = if language == "zh" {
                    ["重启/关停", "SSH断开", "未保存工作丢失", "拒绝"]
                } else {
                    ["Reboot/halt", "SSHdrops", "unsavedworklost", "Deny"]
                };
                phrases.iter().all(|phrase| text.contains(phrase))
            });
            session.send(b"d\n");
            session.wait_screen("irrecoverable Details Enter stays pending", |screen| {
                screen.contents().contains(if language == "zh" {
                    "> [ 详情 ]"
                } else {
                    "> [ Details ]"
                })
            });
            assert!(!session.home().join("reboot-attempt").exists());
            session.send(b"\x1b[D");
            session.wait_screen("irrecoverable denial focus", |screen| {
                screen.contents().contains(if language == "zh" {
                    "> [ 拒绝 ]"
                } else {
                    "> [ Deny ]"
                })
            });
            session.send(b"\n");
            session.wait_screen("irrecoverable denied receipt", |screen| {
                screen.contents().contains(if language == "zh" {
                    "已拒绝 req-1"
                } else {
                    "Denied req-1"
                })
            });
            assert!(!session.home().join("reboot-attempt").exists());
            session.finish();
        }
    }
}

#[test]
fn raw_cli_approval_recording_turn_scope_real_40x16() {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[("bin/ps", "#!/bin/sh\nprintf 'SYNTHETIC_PROCESS_ONLY\\n'\n")],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("COSH_SHELL_WIDTH", "40"),
                    ("PATH", "$HOME/bin:/usr/bin:/bin"),
                ],
                &format!("turn-scope-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(b"?? stream batch tool approval\n");
            session.wait_screen("turn card without focused policy text", |screen| {
                let text = visible_text(screen);
                text.contains(if language == "zh" {
                    "本轮全部允许"
                } else {
                    "Allowallthisturn"
                }) && text.contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            let text = visible_text(session.screen());
            assert!(
                !text.contains(if language == "zh" {
                    "批准当前项"
                } else {
                    "Approvethis."
                }),
                "{text}"
            );
            session.send(b"\x1b[C");
            session.wait_screen("turn action scope visible before consent", |screen| {
                let text = visible_text(screen);
                let phrases = if language == "zh" {
                    [
                        ">[本轮全部允许]",
                        "批准当前项",
                        "已排队",
                        "新到达",
                        "合格Shell工具",
                        "原样输出",
                    ]
                } else {
                    [
                        ">[Allowallthisturn]",
                        "Approvethis.",
                        "queued",
                        "new",
                        "Eligible",
                        "outputas-is",
                    ]
                };
                phrases.iter().all(|phrase| text.contains(phrase))
            });
            session.send(b"d");
            session.wait_screen("turn scope Details", |screen| {
                screen.contents().contains(if language == "zh" {
                    "> [ 详情 ]"
                } else {
                    "> [ Details ]"
                })
            });
            super::details::page_through_details(&mut session, language, true);
            session.send(b"\x1b[D");
            session.wait_screen("turn Details returns to denial", |screen| {
                screen.contents().contains(if language == "zh" {
                    "> [ 拒绝 ]"
                } else {
                    "> [ Deny ]"
                })
            });
            session.send(b"\x1b[D\x1b[D");
            session.wait_screen("turn selected scope restored", |screen| {
                screen.contents().contains(if language == "zh" {
                    "> [ 本轮全部允许 ]"
                } else {
                    "> [ Allow all this turn ]"
                })
            });
            session.send(b"\n");
            session.wait_screen("batched commands finish before follow-up", |screen| {
                visible_text(screen).contains("Commandresultanalysisforreq-3")
            });
            let output = strip_ansi_escape(&session.raw_output());
            for request in ["req-1", "req-2", "req-3"] {
                assert!(
                    output.contains(&format!(
                        "{} {request}",
                        if language == "zh" {
                            "本轮已批准"
                        } else {
                            "Approved for this turn"
                        }
                    )),
                    "{output}"
                );
            }
            session.finish();
        }
    }
}

#[test]
fn raw_cli_approval_recording_environment_negative_real_40x16() {
    for (scenario, command) in [
        ("echo-name", "echo printenv"),
        ("help", "printenv --help"),
        ("env-payload", "env pwd"),
        ("lookup", "command -v printenv"),
    ] {
        record_output_source(scenario, command, "Unpredictable", "无法预判");
    }
}

#[test]
fn raw_cli_approval_recording_long_sensitive_tail_real_40x16() {
    let command = format!("{}printenv TEST_SYNTHETIC_VALUE", "uptime; ".repeat(40));
    record_output_source("long-sensitive-tail", &command, "Environment", "环境变量");
}

fn record_decision(scenario: &str, input: &[u8], english: &str, chinese: &str) {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[],
                &[("COSH_SHELL_LANG", language), ("COSH_SHELL_WIDTH", "40")],
                &format!("decision-{scenario}-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(b"?? approval output fixture: printf '%s\\n' SYNTHETIC_DECISION >> decision-result\n");
            session.wait_screen("decision card", |screen| {
                let text = visible_text(screen);
                text.contains(if language == "zh" {
                    "审批req-1"
                } else {
                    "Approvalreq-1"
                }) && text.contains(if language == "zh" {
                    ">[允许一次]"
                } else {
                    ">[Allowonce]"
                })
            });
            let config = session.home().join(".copilot-shell/config.toml");
            let original_config = fs::read(&config).unwrap();
            session.send(input);
            session.wait_screen("decision receipt", |screen| {
                screen
                    .contents()
                    .contains(if language == "zh" { chinese } else { english })
            });
            if scenario == "allow" {
                session.wait_screen("approved command completes before return", |screen| {
                    visible_text(screen).contains("Commandresultanalysisforreq-1")
                });
                assert_eq!(
                    fs::read(session.home().join("decision-result")).unwrap(),
                    b"SYNTHETIC_DECISION\n"
                );
            } else {
                assert!(!session.home().join("decision-result").exists());
            }
            assert_eq!(fs::read(&config).unwrap(), original_config);
            session.finish();
        }
    }
}

fn record_output_source(scenario: &str, command: &str, english: &str, chinese: &str) {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[(
                    ".copilot-shell/config.toml",
                    "[shell]\nadapter_default = 'fake'\nreadonly_disabled = ['uptime', 'ps', 'printenv', 'echo', 'pwd', 'env', 'command']\n",
                )],
                &[("COSH_SHELL_LANG", language), ("COSH_SHELL_WIDTH", "40")],
                &format!("{scenario}-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(format!("?? approval output fixture: {command}\n").as_bytes());
            let phrase = if language == "zh" { chinese } else { english };
            session.wait_screen("output-source card with denial", |screen| {
                let text = visible_text(screen);
                text.contains(if language == "zh" {
                    "审批req-1"
                } else {
                    "Approvalreq-1"
                }) && text.contains(if language == "zh" { "拒绝" } else { "Deny" })
                    && (phrase.is_empty() || text.contains(phrase))
            });
            if english == "Unpredictable" {
                let content = visible_text(session.screen());
                assert!(
                    !content.contains("Environment") && !content.contains("环境变量"),
                    "{content}"
                );
            }
            if phrase.is_empty() {
                let text = visible_text(session.screen());
                for unexpected in [
                    "原样",
                    "终端",
                    "Environment",
                    "Sensitive",
                    "Unpredictable",
                    "Fullprocessarguments",
                ] {
                    assert!(!text.contains(unexpected), "{scenario}: {text}");
                }
            }
            let config = session.home().join(".copilot-shell/config.toml");
            let original_config = fs::read(&config).unwrap();
            session.send(b"d");
            session.wait_screen("output-source Details view", |screen| {
                visible_text(screen).contains(if language == "zh" {
                    "审批详情req-1"
                } else {
                    "Approvaldetailsreq-1"
                }) && screen.contents().contains(if language == "zh" {
                    "> [ 详情 ]"
                } else {
                    "> [ Details ]"
                })
            });
            super::details::page_through_details(&mut session, language, false);
            session.send(b"\x1b[D");
            session.wait_screen("output-source return to denial", |screen| {
                screen.contents().contains(if language == "zh" {
                    "> [ 拒绝 ]"
                } else {
                    "> [ Deny ]"
                })
            });
            session.send(b"\n");
            session.wait_screen("output-source denied receipt", |screen| {
                screen.contents().contains(if language == "zh" {
                    "已拒绝 req-1"
                } else {
                    "Denied req-1"
                })
            });
            assert_eq!(fs::read(&config).unwrap(), original_config);
            let output = session.raw_output();
            for unexpected in [
                "Approved req-1",
                "Trusted req-1",
                "已批准 req-1",
                "已信任 req-1",
                "Bash tool sent to shell",
                "Bash tool 已发送到 shell",
            ] {
                assert!(!output.contains(unexpected), "{scenario}: {output}");
            }
            session.finish();
        }
    }
}
