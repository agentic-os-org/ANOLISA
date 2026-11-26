use crate::support::terminal_screen::TerminalSession;

fn text(screen: &vt100::Screen) -> String {
    screen
        .contents()
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '│')
        .collect()
}

#[test]
fn raw_cli_approval_recording_plain_real_40x16() {
    record_viewport(40, 16, true);
}

#[test]
fn raw_cli_approval_recording_wide_real_80x24() {
    record_viewport(80, 24, false);
}

#[test]
fn raw_cli_approval_recording_short_real_80x16() {
    record_viewport(80, 16, false);
}

#[test]
fn raw_cli_approval_recording_resize_and_arrow_details_real_40x16() {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                80,
                &[],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("TEST_SYNTHETIC_VALUE", "SYNTHETIC_RESIZE_MUST_NOT_RUN"),
                ],
                &format!("resize-arrow-details-{shell}-{language}"),
            );
            session.send(b"?? approval output fixture: printenv TEST_SYNTHETIC_VALUE\n");
            session.wait_screen("80x24 approval before resize", |screen| {
                text(screen).contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            session.send(b"\x1b[C\x1b[C\x1b[C\n");
            session.wait_screen(
                "arrow Details Enter remains pending before resize",
                |screen| {
                    text(screen).contains(if language == "zh" {
                        ">[详情]"
                    } else {
                        ">[Details]"
                    })
                },
            );
            session.resize_to(16, 40);
            session.wait_screen("resized Details focus and denial fit 40x16", |screen| {
                let content = text(screen);
                content.contains(if language == "zh" {
                    "审批详情req-1"
                } else {
                    "Approvaldetailsreq-1"
                }) && content.contains(if language == "zh" {
                    ">[详情]"
                } else {
                    ">[Details]"
                }) && content.contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            super::details::page_through_details(&mut session, language, false);
            session.send(b"\x1b[D\n");
            session.wait_screen("resized arrow Details returns to denial", |screen| {
                text(screen).contains(if language == "zh" {
                    "已拒绝req-1"
                } else {
                    "Deniedreq-1"
                })
            });
            assert!(!session
                .raw_output()
                .contains("SYNTHETIC_RESIZE_MUST_NOT_RUN"));
            session.finish();
        }
    }
}

#[test]
fn raw_cli_approval_recording_height_resize_restarts_details_without_skips() {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let command = (1..=64)
                .map(|number| format!("echo PAGE_ROW_{number:02}"))
                .collect::<Vec<_>>()
                .join("; ");
            let command = format!("{command}; touch resized-details-executed");
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                80,
                &[],
                &[("COSH_SHELL_LANG", language)],
                &format!("height-resize-details-{shell}-{language}"),
            );
            session.resize_to(16, 80);
            session.send(format!("?? approval output fixture: {command}\n").as_bytes());
            session.wait_screen("80x16 long command approval", |screen| {
                text(screen).contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            session.send(b"d\n");
            session.wait_screen("80x16 first Details page stays pending", |screen| {
                let content = text(screen);
                content.contains("PAGE_ROW_01")
                    && content.contains(if language == "zh" {
                        ">[详情]"
                    } else {
                        ">[Details]"
                    })
            });
            session.send(b"\x1b[B");
            session.wait_screen("80x16 second Details page", |screen| {
                text(screen).contains(if language == "zh" { "第2/" } else { "Page2/" })
            });
            session.resize_to(24, 80);
            session.wait_screen(
                "height resize restarts Details at first source line",
                |screen| {
                    let content = text(screen);
                    content.contains("PAGE_ROW_01")
                        && content.contains(if language == "zh" { "第1/" } else { "Page1/" })
                        && content.contains(if language == "zh" {
                            ">[详情]"
                        } else {
                            ">[Details]"
                        })
                },
            );
            let body = |screen: &vt100::Screen| {
                screen
                    .contents()
                    .lines()
                    .skip(3)
                    .take_while(|line| {
                        !line
                            .trim()
                            .trim_matches('│')
                            .trim()
                            .starts_with(if language == "zh" { "第 " } else { "Page " })
                    })
                    .flat_map(|line| {
                        line.chars()
                            .filter(|character| !character.is_whitespace() && *character != '│')
                    })
                    .collect::<String>()
            };
            let mut seen = body(session.screen());
            let (_, pages) =
                super::details::details_page_position(session.screen(), language).unwrap();
            for page in 2..=pages {
                session.send(b"\x1b[B");
                session.wait_screen("resized Details continuation remains focused", |screen| {
                    super::details::details_page_position(screen, language) == Some((page, pages))
                        && text(screen).contains(if language == "zh" {
                            ">[详情]"
                        } else {
                            ">[Details]"
                        })
                });
                seen.push_str(&body(session.screen()));
            }
            for number in 1..=64 {
                assert!(
                    seen.contains(&format!("PAGE_ROW_{number:02}")),
                    "missing {number}: {seen}"
                );
            }
            session.send(b"\n");
            session.wait_screen("resized Details Enter does not execute", |screen| {
                text(screen).contains(if language == "zh" {
                    ">[详情]"
                } else {
                    ">[Details]"
                })
            });
            session.send(b"\x1b[D");
            session.wait_screen("height resized Details returns to denial focus", |screen| {
                text(screen).contains(if language == "zh" {
                    ">[拒绝]"
                } else {
                    ">[Deny]"
                })
            });
            session.send(b"\n");
            session.wait_screen("height resized denial returns to shell", |screen| {
                screen.contents().trim_end().ends_with("screen$")
            });
            let raw = crate::support::raw_cli::strip_ansi_escape(&session.raw_output());
            assert!(raw.contains(if language == "zh" {
                "已拒绝 req-1"
            } else {
                "Denied req-1"
            }));
            assert!(!session.home().join("resized-details-executed").exists());
            session.finish();
        }
    }
}

fn record_viewport(cols: u16, rows: u16, plain: bool) {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                cols,
                &[],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("COSH_SHELL_RENDER", if plain { "plain" } else { "ratatui" }),
                    ("TEST_SYNTHETIC_VALUE", "SYNTHETIC_VIEWPORT_MUST_NOT_RUN"),
                ],
                &format!(
                    "viewport-{shell}-{language}-{cols}x{rows}-{}",
                    if plain { "plain" } else { "color" }
                ),
            );
            session.resize_to(rows, cols);
            session.send(b"?? approval output fixture: printenv TEST_SYNTHETIC_VALUE\n");
            session.wait_screen("viewport approval warning and denial", |screen| {
                let content = text(screen);
                content.contains(if language == "zh" {
                    "环境变量"
                } else {
                    "Environment"
                }) && content.contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            session.send(b"d\n");
            session.wait_screen("viewport Details Enter remains pending", |screen| {
                let content = text(screen);
                content.contains(if language == "zh" {
                    "审批详情req-1"
                } else {
                    "Approvaldetailsreq-1"
                }) && content.contains(if language == "zh" {
                    ">[详情]"
                } else {
                    ">[Details]"
                }) && content.contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            session.send(b"\x1b[D");
            let deny_focus = match (language, plain) {
                ("zh", true) => "[拒绝]",
                ("zh", false) => ">[拒绝]",
                (_, true) => "[Deny]",
                (_, false) => ">[Deny]",
            };
            session.wait_screen("viewport return to denial", |screen| {
                let content = text(screen);
                content.contains(deny_focus)
                    && !content.contains(if language == "zh" {
                        "审批详情req-1"
                    } else {
                        "Approvaldetailsreq-1"
                    })
            });
            session.send(b"\n");
            session.wait_screen("viewport denied receipt", |screen| {
                text(screen).contains(if language == "zh" {
                    "已拒绝req-1"
                } else {
                    "Deniedreq-1"
                })
            });
            assert!(!session
                .raw_output()
                .contains("SYNTHETIC_VIEWPORT_MUST_NOT_RUN"));
            session.finish();
        }
    }
}
