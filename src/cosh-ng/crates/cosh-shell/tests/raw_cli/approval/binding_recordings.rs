use super::*;
use crate::support::terminal_screen::TerminalSession;

fn text(screen: &vt100::Screen) -> String {
    screen
        .contents()
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '│')
        .collect()
}

#[test]
fn raw_cli_approval_recording_command_binding_real_40x16() {
    record_binding(
        "command",
        "Bash",
        r#"{"command":"printenv TEST_SYNTHETIC_VALUE"}"#,
        true,
    );
}

#[test]
fn raw_cli_approval_recording_cmd_binding_real_40x16() {
    record_binding(
        "cmd",
        "run_shell_command",
        r#"{"cmd":"printenv TEST_SYNTHETIC_VALUE"}"#,
        true,
    );
}

#[test]
fn raw_cli_approval_recording_string_binding_real_40x16() {
    record_binding(
        "string",
        "shell",
        r#""printenv TEST_SYNTHETIC_VALUE""#,
        true,
    );
}

#[test]
fn raw_cli_approval_recording_conflicting_binding_real_40x16() {
    record_binding(
        "conflicting",
        "bash",
        r#"{"command":"printenv TEST_SYNTHETIC_VALUE","cmd":"uptime"}"#,
        true,
    );
}

#[test]
fn raw_cli_approval_recording_missing_binding_real_40x16() {
    record_binding("missing", "Bash", r#"{"description":"uptime"}"#, false);
}

fn record_binding(scenario: &str, tool: &str, input: &str, environment: bool) {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[],
                &[("COSH_SHELL_LANG", language), ("COSH_SHELL_WIDTH", "40")],
                &format!("binding-{scenario}-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(format!("?? approval typed fixture: {tool} ::: {input}\n").as_bytes());
            let warning = match (language, environment) {
                ("zh", true) => "环境变量",
                ("zh", false) => "无法预判",
                (_, true) => "Environment",
                (_, false) => "Unpredictable",
            };
            session.wait_screen("bound-command disclosure", |screen| {
                let content = text(screen);
                content.contains(warning)
                    && content.contains(if language == "zh" { "拒绝" } else { "Deny" })
            });
            session.send(b"d\n");
            session.wait_screen("bound-command Details pending", |screen| {
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
            session.send(b"\x1b[D\n");
            session.wait_screen("bound-command denied", |screen| {
                text(screen).contains(if language == "zh" {
                    "已拒绝req-1"
                } else {
                    "Deniedreq-1"
                })
            });
            let output = strip_ansi_escape(&session.raw_output());
            assert!(!output.contains("Bash tool sent to shell"), "{output}");
            assert!(!output.contains("Bash tool 已发送到 shell"), "{output}");
            session.finish();
        }
    }
}
