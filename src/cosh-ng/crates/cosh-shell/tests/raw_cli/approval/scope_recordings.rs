use super::*;
use crate::support::terminal_screen::TerminalSession;

const CORE_FIXTURE: &str = r#"#!/bin/sh
set -eu
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"approval-scope-fixture","model":"synthetic-control-protocol"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"scope-write","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"synthetic-write","content":"synthetic"},"tool_use_id":"scope-write-tool","hook_requires_approval":true}}'
printf '%s\n' '{"type":"control_request","request_id":"scope-queued","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-scope queued"},"tool_use_id":"scope-queued-tool"}}'
queued_done=0
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"host_executed_shell"'*'scope-queued'*) queued_done=1; break ;;
  esac
done
[ "$queued_done" = 1 ]
printf '%s\n' '{"type":"control_request","request_id":"scope-late","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-scope late"},"tool_use_id":"scope-late-tool"}}'
late_done=0
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"host_executed_shell"'*'scope-late'*) late_done=1; break ;;
  esac
done
[ "$late_done" = 1 ]
printf '%s\n' '{"type":"control_request","request_id":"scope-high","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-scope high > synthetic-high"},"tool_use_id":"scope-high-tool"}}'
printf '%s\n' '{"type":"control_request","request_id":"scope-hook","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-scope hook"},"tool_use_id":"scope-hook-tool","hook_requires_approval":true}}'
printf '%s\n' '{"type":"control_request","request_id":"scope-missing","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"description":"missing binding"},"tool_use_id":"scope-missing-tool"}}'
denied=0
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"deny"'*) denied=$((denied + 1)); [ "$denied" -eq 3 ] && break ;;
    *'"behavior":"host_executed_shell"'*) exit 1 ;;
  esac
done
[ "$denied" -eq 3 ]
printf '%s\n' '{"type":"assistant","session_id":"approval-scope-fixture","message":{"content":[{"type":"text","text":"SYNTHETIC_SCOPE_COMPLETED"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"approval-scope-fixture","is_error":false,"result":"done"}'
"#;

fn text(screen: &vt100::Screen) -> String {
    screen
        .contents()
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '│')
        .collect()
}

#[test]
fn raw_cli_approval_recording_hook_notice_with_turn_scope_real_40x16() {
    record_hook_notice("git status; printenv; ps aux", false, 40);
}

#[test]
fn raw_cli_approval_recording_hook_notice_with_turn_scope_real_41x16() {
    record_hook_notice("git status; printenv; ps aux", false, 41);
}

#[test]
fn raw_cli_approval_recording_irrecoverable_hook_notice_real_40x16() {
    record_hook_notice("printenv; ps aux; reboot", true, 40);
}

fn record_hook_notice(command: &str, irrecoverable: bool, cols: u16) {
    let provider = r#"#!/bin/sh
set -eu
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"approval-hook-fixture","model":"synthetic-control-protocol"}'
read -r user_message
printf '%s\n' '{"type":"system","subtype":"hook_notification","hook_name":"audit","status":"Review requested before running; check approval policy and output handling.","decision":"ask","tool_use_id":"hook-notice-tool"}'
printf '%s\n' '{"type":"system","subtype":"hook_notification","hook_name":"policy","status":"Synthetic policy notice only.","decision":"allow","tool_use_id":"hook-notice-tool"}'
printf '%s\n' '{"type":"system","subtype":"hook_notification","hook_name":"output","status":"Synthetic output notice only.","decision":"allow","tool_use_id":"hook-notice-tool"}'
printf '%s\n' '{"type":"control_request","request_id":"hook-notice","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"git status; printenv; ps aux"},"tool_use_id":"hook-notice-tool","hook_requires_approval":true}}'
printf '%s\n' '{"type":"control_request","request_id":"hook-queued","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-inspect"},"tool_use_id":"hook-queued-tool"}}'
denied=0
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"deny"'*) denied=$((denied + 1)); [ "$denied" -eq 2 ] && break ;;
    *'"behavior":"host_executed_shell"'*) exit 1 ;;
  esac
done
printf '%s\n' '{"type":"assistant","session_id":"approval-hook-fixture","message":{"content":[{"type":"text","text":"SYNTHETIC_HOOK_DENIED"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"approval-hook-fixture","is_error":false,"result":"done"}'
"#;
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let width = cols.to_string();
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                cols,
                &[
                    (
                        ".copilot-shell/config.toml",
                        "[shell]\nadapter_default = 'cosh-core'\n",
                    ),
                    (
                        "bin/cosh-core",
                        &provider.replace("git status; printenv; ps aux", command),
                    ),
                ],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("COSH_SHELL_WIDTH", width.as_str()),
                    ("COSH_SHELL_ADAPTER", "cosh-core"),
                    ("COSH_CORE_PATH", "$HOME/bin/cosh-core"),
                ],
                &format!(
                    "hook-notice-{}-{shell}-{language}-{cols}x16",
                    if irrecoverable {
                        "irrecoverable"
                    } else {
                        "turn"
                    }
                ),
            );
            session.resize_to(16, cols);
            session.send(b"?? approval hook fixture\n");
            session.wait_screen("hook notice and denial on main card", |screen| {
                let content = text(screen);
                content.contains(if irrecoverable {
                    "audit"
                } else {
                    "Reviewrequested"
                }) && content.contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            session.send(b"\x1b[C");
            session.wait_screen("hook risk, command, scope and denial fit 40x16", |screen| {
                let content = text(screen);
                let phrases = if language == "zh" {
                    [
                        "审批req-1",
                        "高风险",
                        if irrecoverable {
                            "printenv"
                        } else {
                            "gitstatus"
                        },
                        "audit",
                        ">[本轮全部允许]",
                        "拒绝",
                    ]
                } else {
                    [
                        "Approvalreq-1",
                        "highrisk",
                        if irrecoverable {
                            "printenv"
                        } else {
                            "gitstatus"
                        },
                        "audit",
                        ">[Allowallthisturn]",
                        "Deny",
                    ]
                };
                phrases.iter().all(|phrase| content.contains(phrase))
            });
            session.send(b"d\n");
            session.wait_screen("hook Details focus remains pending", |screen| {
                let content = text(screen);
                content.contains(if language == "zh" {
                    "审批详情req-1"
                } else {
                    "Approvaldetailsreq-1"
                }) && content.contains(if language == "zh" {
                    ">[详情]"
                } else {
                    ">[Details]"
                })
            });
            super::details::page_through_details(&mut session, language, true);
            session.send(b"\x1b[D\n");
            session.wait_screen("hook request denied without execution", |screen| {
                text(screen).contains(if language == "zh" {
                    "已拒绝req-1"
                } else {
                    "Deniedreq-1"
                })
            });
            session.send(b"\x1b");
            session.wait_screen("hook provider confirms both denials", |screen| {
                text(screen).contains("SYNTHETIC_HOOK_DENIED")
            });
            let output = strip_ansi_escape(&session.raw_output());
            assert!(!output.contains("Bash tool sent to shell"), "{output}");
            assert!(!output.contains("Bash tool 已发送到 shell"), "{output}");
            session.finish();
        }
    }
}

#[test]
fn raw_cli_approval_recording_non_shell_scope_and_exclusions_real_40x16() {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[
                    (
                        "bin/synthetic-scope",
                        "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$HOME/scope-result\"\n",
                    ),
                    (
                        ".copilot-shell/config.toml",
                        "[shell]\nadapter_default = 'cosh-core'\n",
                    ),
                    ("bin/cosh-core", CORE_FIXTURE),
                ],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("COSH_SHELL_WIDTH", "40"),
                    ("PATH", "$HOME/bin:/usr/bin:/bin"),
                    ("COSH_SHELL_ADAPTER", "cosh-core"),
                    ("COSH_CORE_PATH", "$HOME/bin/cosh-core"),
                ],
                &format!("non-shell-scope-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(b"?? approval scope fixture\n");
            session.wait_screen("non-Shell first card with turn action", |screen| {
                let content = text(screen);
                content.contains("Write")
                    && content.contains(if language == "zh" {
                        "本轮全部允许"
                    } else {
                        "Allowallthisturn"
                    })
            });
            session.send(b"\x1b[C");
            session.wait_screen("non-Shell turn scope before consent", |screen| {
                let content = text(screen);
                content.contains(if language == "zh" {
                    ">[本轮全部允许]"
                } else {
                    ">[Allowallthisturn]"
                }) && content.contains(if language == "zh" {
                    "同轮已排队、新到达"
                } else {
                    "Eligiblequeued/new"
                })
            });
            session.send(b"\n");
            let result_path = session.home().join("scope-result");
            session.wait_screen(
                "late eligible request covered, high risk still pending",
                |screen| {
                    let content = text(screen);
                    content.contains(if language == "zh" {
                        "审批req-4"
                    } else {
                        "Approvalreq-4"
                    }) && fs::read_to_string(&result_path)
                        .is_ok_and(|result| result == "queued\nlate\n")
                },
            );
            assert_eq!(
                fs::read_to_string(session.home().join("scope-result")).unwrap(),
                "queued\nlate\n"
            );
            for request in ["req-4", "req-5", "req-6"] {
                session.send(b"\x1b");
                session.wait_screen("excluded request explicitly cancelled", |screen| {
                    text(screen).contains(&format!(
                        "{}{request}",
                        if language == "zh" {
                            "已取消"
                        } else {
                            "Cancelled"
                        }
                    ))
                });
                let output = strip_ansi_escape(&session.raw_output());
                assert!(
                    output.contains(&format!(
                        "{} {request}",
                        if language == "zh" {
                            "已取消"
                        } else {
                            "Cancelled"
                        }
                    )),
                    "{output}"
                );
            }
            assert_eq!(
                fs::read_to_string(session.home().join("scope-result")).unwrap(),
                "queued\nlate\n"
            );
            session.wait_screen("provider confirms all exclusions", |screen| {
                text(screen).contains("SYNTHETIC_SCOPE_COMPLETED")
            });
            session.finish();
        }
    }
}

#[test]
fn raw_cli_approval_recording_details_focus_survives_later_queue_real_40x16() {
    let provider = r#"#!/bin/sh
set -eu
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"approval-queue-fixture","model":"synthetic-control-protocol"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"queue-first","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"printenv TEST_SYNTHETIC_VALUE"},"tool_use_id":"queue-first-tool"}}'
while [ ! -f "$HOME/queue-release" ]; do sleep 0.01; done
printf '%s\n' '{"type":"control_request","request_id":"queue-second","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-inspect"},"tool_use_id":"queue-second-tool"}}'
denied=0
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"deny"'*) denied=$((denied + 1)); [ "$denied" -eq 2 ] && break ;;
    *'"behavior":"host_executed_shell"'*) exit 1 ;;
  esac
done
printf '%s\n' '{"type":"assistant","session_id":"approval-queue-fixture","message":{"content":[{"type":"text","text":"SYNTHETIC_QUEUE_DENIED"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"approval-queue-fixture","is_error":false,"result":"done"}'
"#;
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[
                    (
                        ".copilot-shell/config.toml",
                        "[shell]\nadapter_default = 'cosh-core'\n",
                    ),
                    ("bin/cosh-core", provider),
                ],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("COSH_SHELL_WIDTH", "40"),
                    ("COSH_SHELL_ADAPTER", "cosh-core"),
                    ("COSH_CORE_PATH", "$HOME/bin/cosh-core"),
                    ("TEST_SYNTHETIC_VALUE", "SYNTHETIC_QUEUE_MUST_NOT_RUN"),
                ],
                &format!("later-queue-details-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(b"?? approval queue fixture\n");
            session.wait_screen("first queue card without batch action", |screen| {
                let content = text(screen);
                content.contains(if language == "zh" {
                    "审批req-1"
                } else {
                    "Approvalreq-1"
                }) && !content.contains(if language == "zh" {
                    "本轮全部允许"
                } else {
                    "Allowallthisturn"
                })
            });
            session.send(b"d\n");
            session.wait_screen("Details pending before queue update", |screen| {
                text(screen).contains(if language == "zh" {
                    ">[详情]"
                } else {
                    ">[Details]"
                })
            });
            fs::write(
                session.home().join("queue-release"),
                "synthetic queue trigger",
            )
            .unwrap();
            session.wait_screen(
                "later queue adds batch action without moving Details focus",
                |screen| {
                    let content = text(screen);
                    content.contains(if language == "zh" {
                        ">[详情]"
                    } else {
                        ">[Details]"
                    }) && content.contains(if language == "zh" {
                        "本轮全部允许"
                    } else {
                        "Allowallthisturn"
                    })
                },
            );
            session.send(b"\n");
            session.wait_screen("Enter after queue update remains pending", |screen| {
                text(screen).contains(if language == "zh" {
                    ">[详情]"
                } else {
                    ">[Details]"
                })
            });
            super::details::page_through_details(&mut session, language, true);
            session.send(b"\x1b[D\n");
            session.wait_screen("first queued request denied", |screen| {
                text(screen).contains(if language == "zh" {
                    "已拒绝req-1"
                } else {
                    "Deniedreq-1"
                })
            });
            session.send(b"\x1b");
            session.wait_screen("queue provider confirms no execution", |screen| {
                text(screen).contains("SYNTHETIC_QUEUE_DENIED")
            });
            assert!(!session
                .raw_output()
                .contains("SYNTHETIC_QUEUE_MUST_NOT_RUN"));
            session.finish();
        }
    }
}

#[test]
fn raw_cli_approval_recording_turn_scope_does_not_replay_failed_effect_real_40x16() {
    let provider = r#"#!/bin/sh
set -eu
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"approval-retry-fixture","model":"synthetic-control-protocol"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"retry-first","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-retry"},"tool_use_id":"retry-first-tool"}}'
printf '%s\n' '{"type":"control_request","request_id":"retry-queued","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-retry queued"},"tool_use_id":"retry-queued-tool"}}'
completed=0
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"host_executed_shell"'*) completed=$((completed + 1)); [ "$completed" -eq 2 ] && break ;;
  esac
done
printf '%s\n' '{"type":"control_request","request_id":"retry-later","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-retry"},"tool_use_id":"retry-later-tool"}}'
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"deny"'*) break ;;
    *'"behavior":"host_executed_shell"'*) exit 1 ;;
  esac
done
printf '%s\n' '{"type":"assistant","session_id":"approval-retry-fixture","message":{"content":[{"type":"text","text":"SYNTHETIC_RETRY_DENIED"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"approval-retry-fixture","is_error":false,"result":"done"}'
"#;
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell, "enhanced", 40,
                &[
                    (".copilot-shell/config.toml", "[shell]\nadapter_default = 'cosh-core'\n"),
                    ("bin/cosh-core", provider),
                    ("bin/synthetic-retry", "#!/bin/sh\nif [ \"${1-}\" = queued ]; then printf 'queued\\n' >> \"$HOME/retry-result\"; exit 0; fi\nprintf 'failed\\n' >> \"$HOME/retry-result\"\nexit 1\n"),
                ],
                &[("COSH_SHELL_LANG", language), ("COSH_SHELL_WIDTH", "40"), ("PATH", "$HOME/bin:/usr/bin:/bin"), ("COSH_SHELL_ADAPTER", "cosh-core"), ("COSH_CORE_PATH", "$HOME/bin/cosh-core")],
                &format!("blocked-retry-turn-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(b"?? approval retry fixture\n");
            session.wait_screen("turn consent available before failed effect", |screen| {
                text(screen).contains(if language == "zh" {
                    "本轮全部允许"
                } else {
                    "Allowallthisturn"
                })
            });
            session.send(b"\x1b[C\n");
            let result_path = session.home().join("retry-result");
            session.wait_screen("failed effect retry retains approval card", |screen| {
                text(screen).contains(if language == "zh" {
                    "审批req-3"
                } else {
                    "Approvalreq-3"
                }) && fs::read_to_string(&result_path)
                    .is_ok_and(|result| result == "failed\nqueued\n")
            });
            session.send(b"d\n");
            session.wait_screen("failed retry Details remains pending", |screen| {
                text(screen).contains(if language == "zh" {
                    ">[详情]"
                } else {
                    ">[Details]"
                })
            });
            super::details::page_through_details(&mut session, language, true);
            session.send(b"\x1b[D\n");
            session.wait_screen("retry denied without replay", |screen| {
                text(screen).contains("SYNTHETIC_RETRY_DENIED")
            });
            assert_eq!(
                fs::read_to_string(session.home().join("retry-result")).unwrap(),
                "failed\nqueued\n"
            );
            session.finish();
        }
    }
}
