use super::*;
use crate::support::terminal_screen::TerminalSession;

const CORE_FIXTURE: &str = r#"#!/bin/sh
set -eu
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"approval-mode-fixture","model":"synthetic-control-protocol"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"mode-tool","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"uptime"},"tool_use_id":"mode-tool-use"}}'
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"host_executed_shell"'*) break ;;
  esac
done
printf '%s\n' '{"type":"assistant","session_id":"approval-mode-fixture","message":{"content":[{"type":"text","text":"SYNTHETIC_MODE_COMPLETED"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"approval-mode-fixture","is_error":false,"result":"done"}'
"#;

#[test]
fn raw_cli_approval_recording_auto_no_card_real_40x16() {
    record_mode("auto", "");
}

#[test]
fn raw_cli_approval_recording_trust_no_card_real_40x16() {
    record_mode("trust", "");
}

#[test]
fn raw_cli_approval_recording_exact_command_trust_real_40x16() {
    record_mode(
        "auto",
        "readonly_disabled = ['uptime']\ntrusted_commands = ['uptime']\n",
    );
}

#[test]
fn raw_cli_approval_recording_exact_trust_choice_covers_only_matching_command() {
    let provider = r#"#!/bin/sh
set -eu
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"approval-trust-fixture","model":"synthetic-control-protocol"}'
read -r user_message
for request in trust-first trust-second; do
  printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"$request\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"shell\",\"input\":{\"command\":\"synthetic-trust\"},\"tool_use_id\":\"$request-tool\"}}"
  while IFS= read -r response; do
    case "$response" in
      *'"behavior":"host_executed_shell"'*) break ;;
      *'"behavior":"deny"'*) exit 1 ;;
    esac
  done
done
printf '%s\n' '{"type":"control_request","request_id":"trust-other","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-trust other"},"tool_use_id":"trust-other-tool"}}'
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"deny"'*) break ;;
    *'"behavior":"host_executed_shell"'*) exit 1 ;;
  esac
done
printf '%s\n' '{"type":"assistant","session_id":"approval-trust-fixture","message":{"content":[{"type":"text","text":"SYNTHETIC_TRUST_COMPLETED"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"approval-trust-fixture","is_error":false,"result":"done"}'
"#;
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell, "enhanced", 40,
                &[
                    (".copilot-shell/config.toml", "[shell]\nadapter_default = 'cosh-core'\n"),
                    ("bin/cosh-core", provider),
                    ("bin/synthetic-trust", "#!/bin/sh\nprintf 'executed\\n' >> \"$HOME/trust-result\"\nprintf 'SYNTHETIC_TRUST_OUTPUT\\n'\n"),
                ],
                &[("COSH_SHELL_LANG", language), ("COSH_SHELL_WIDTH", "40"), ("PATH", "$HOME/bin:/usr/bin:/bin"), ("COSH_SHELL_ADAPTER", "cosh-core"), ("COSH_CORE_PATH", "$HOME/bin/cosh-core")],
                &format!("exact-trust-choice-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(b"?? approval trust fixture\n");
            session.wait_screen("first exact command requires trust choice", |screen| {
                screen.contents().contains(if language == "zh" {
                    "始终信任此命令"
                } else {
                    "Always trust"
                })
            });
            session.send(b"\x1b[C\n");
            let result_path = session.home().join("trust-result");
            session.wait_screen(
                "matching request exempt, different command still pending",
                |screen| {
                    screen.contents().contains(if language == "zh" {
                        "审批 req-3"
                    } else {
                        "Approval req-3"
                    }) && fs::read_to_string(&result_path)
                        .is_ok_and(|result| result == "executed\nexecuted\n")
                },
            );
            session.send(b"\x1b");
            session.wait_screen(
                "unmatched command cancelled with trust retained",
                |screen| screen.contents().contains("SYNTHETIC_TRUST_COMPLETED"),
            );
            assert_eq!(
                fs::read_to_string(session.home().join("trust-result")).unwrap(),
                "executed\nexecuted\n"
            );
            session.finish();
        }
    }
}

fn record_mode(mode: &str, config: &str) {
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let config = format!(
                "[shell]\nadapter_default = 'cosh-core'\napproval_mode = '{mode}'\n{config}"
            );
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell,
                "enhanced",
                40,
                &[
                    (".copilot-shell/config.toml", &config),
                    ("bin/cosh-core", CORE_FIXTURE),
                    ("bin/uptime", "#!/bin/sh\nprintf 'executed\\n' >> \"$HOME/mode-result\"\nprintf 'SYNTHETIC_UPTIME_ONLY\\n'\n"),
                ],
                &[
                    ("COSH_SHELL_LANG", language),
                    ("COSH_SHELL_WIDTH", "40"),
                    ("PATH", "$HOME/bin:/usr/bin:/bin"),
                    ("COSH_SHELL_ADAPTER", "cosh-core"),
                    ("COSH_CORE_PATH", "$HOME/bin/cosh-core"),
                ],
                &format!("mode-{mode}-{shell}-{language}-40x16-{}", if config.contains("trusted_commands") { "exact" } else { "default" }),
            );
            session.resize_to(16, 40);
            session.send(b"?? approval mode fixture\n");
            session.wait_screen("mode completed without approval prompt", |screen| {
                screen.contents().contains("SYNTHETIC_MODE_COMPLETED")
            });
            let output = strip_ansi_escape(&session.raw_output());
            for unwanted in [
                "[ Allow once ]",
                "[ 允许一次 ]",
                "Approval req-1",
                "审批 req-1",
            ] {
                assert!(!output.contains(unwanted), "{output}");
            }
            assert_eq!(
                fs::read_to_string(session.home().join("mode-result")).unwrap(),
                "executed\n"
            );
            session.finish();
        }
    }
}
