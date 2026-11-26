use super::*;
use crate::support::terminal_screen::TerminalSession;

#[test]
fn raw_cli_approval_recording_preview_included_real_40x16() {
    record_preview("included", "preview_included");
}

#[test]
fn raw_cli_approval_recording_preview_home_replacement_real_40x16() {
    record_preview("home", "preview_redacted");
}

#[test]
fn raw_cli_approval_recording_preview_truncation_real_40x16() {
    record_preview("long", "preview_redacted");
}

fn record_preview(scenario: &str, expected_status: &str) {
    let provider = r#"#!/bin/sh
set -eu
read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"approval-preview-fixture","model":"synthetic-control-protocol"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"preview-tool","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"synthetic-preview SCENARIO"},"tool_use_id":"preview-tool-use"}}'
while IFS= read -r response; do
  case "$response" in
    *'"behavior":"host_executed_shell"'*) printf '%s\n' "$response" > "$HOME/provider-preview-response.json"; break ;;
  esac
done
case "$response" in
  *'"redaction_status":"EXPECTED_STATUS"'*) ;;
  *) exit 1 ;;
esac
printf '%s\n' '{"type":"assistant","session_id":"approval-preview-fixture","message":{"content":[{"type":"text","text":"SYNTHETIC_PREVIEW_CONFIRMED EXPECTED_STATUS"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"approval-preview-fixture","is_error":false,"result":"done"}'
"#;
    let provider = provider
        .replace("SCENARIO", scenario)
        .replace("EXPECTED_STATUS", expected_status);
    for shell in ["bash", "zsh"] {
        for language in ["en", "zh"] {
            let mut session = TerminalSession::spawn_for_shell_with_recording(
                shell, "enhanced", 40,
                &[
                    (".copilot-shell/config.toml", "[shell]\nadapter_default = 'cosh-core'\n"),
                    ("bin/cosh-core", &provider),
                    ("bin/synthetic-preview", "#!/bin/sh\ncase \"$1\" in\nincluded) printf 'SYNTHETIC_UNRECOGNIZED_VALUE\\n' ;;\nhome) printf '%s/synthetic-public\\n' \"$HOME\" ;;\nlong) count=0; while [ \"$count\" -lt 8000 ]; do printf x; count=$((count + 1)); done; printf '\\nSYNTHETIC_LONG_TAIL\\n' ;;\nesac\n"),
                ],
                &[("COSH_SHELL_LANG", language), ("COSH_SHELL_WIDTH", "40"), ("PATH", "$HOME/bin:/usr/bin:/bin"), ("COSH_SHELL_ADAPTER", "cosh-core"), ("COSH_CORE_PATH", "$HOME/bin/cosh-core")],
                &format!("preview-{scenario}-{shell}-{language}-40x16"),
            );
            session.resize_to(16, 40);
            session.send(b"?? approval preview fixture\n");
            session.wait_screen("preview source requires explicit approval", |screen| {
                screen.contents().contains(if language == "zh" {
                    "审批 req-1"
                } else {
                    "Approval req-1"
                })
            });
            session.send(b"d\n");
            session.wait_screen(
                "preview boundary Details before terminal execution",
                |screen| {
                    screen.contents().contains(if language == "zh" {
                        "> [ 详情 ]"
                    } else {
                        "> [ Details ]"
                    })
                },
            );
            super::details::page_through_details(&mut session, language, false);
            session.send(b"\x1b[D\x1b[D\x1b[D\n");
            session.wait_screen(
                "provider receives actual bounded preview metadata",
                |screen| screen.contents().contains("SYNTHETIC_PREVIEW_CONFIRMED"),
            );
            let payload: serde_json::Value = serde_json::from_slice(
                &fs::read(session.home().join("provider-preview-response.json")).unwrap(),
            )
            .unwrap();
            let result = &payload["response"]["response"]["result"];
            assert_eq!(result["metadata"]["redaction_status"], expected_status);
            let model = result["llmContent"].as_str().unwrap();
            let preview = model.split_once("bounded_output_summary:\n").unwrap().1;
            let output = session.raw_output();
            match scenario {
                "included" => {
                    assert!(output.contains("SYNTHETIC_UNRECOGNIZED_VALUE"));
                    assert!(model.contains("SYNTHETIC_UNRECOGNIZED_VALUE"));
                }
                "home" => {
                    assert!(
                        output.contains(&format!("{}/synthetic-public", session.home().display()))
                    );
                    assert!(preview.contains("~/synthetic-public"));
                    assert!(!preview.contains(&session.home().display().to_string()));
                }
                _ => {
                    assert!(strip_ansi_escape(&output)
                        .replace(['\r', '\n'], "")
                        .contains(&"x".repeat(8000)));
                    assert!(model.contains("... <truncated"));
                    assert!(!model.contains(&"x".repeat(8000)));
                    assert!(model.contains("SYNTHETIC_LONG_TAIL"));
                }
            }
            session.finish();
        }
    }
}
