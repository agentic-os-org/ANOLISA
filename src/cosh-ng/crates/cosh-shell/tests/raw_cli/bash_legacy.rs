use super::support::terminal_screen::TerminalSession;
use super::*;

const LEGACY_CORE: &str = r#"#!/bin/sh
case " $* " in
  *' --registry '*)
    IFS= read -r request
    request_id=$(printf '%s' "$request" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
    printf '%s\n' "{\"type\":\"registry_response\",\"request_id\":\"$request_id\",\"success\":true,\"data\":{\"model\":\"legacy-fixture\",\"configured\":true}}"
    exit 0
    ;;
esac
IFS= read -r initialize
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"legacy-fixture","model":"legacy-fixture"}'
while IFS= read -r input; do
  case "$input" in
    *'"type":"registry_request"'*)
      request_id=$(printf '%s' "$input" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
      printf '%s\n' "{\"type\":\"registry_response\",\"request_id\":\"$request_id\",\"success\":true,\"data\":{\"model\":\"legacy-fixture\",\"configured\":true}}"
      continue
      ;;
    *'解释一下磁盘用量'*) answer='CHINESE_PROVIDER_COMPLETED'; tag='chinese' ;;
    *'Explain disk usage'*) answer='ENGLISH_PROVIDER_COMPLETED'; tag='english' ;;
    *) continue ;;
  esac
  printf '%s\n' "{\"type\":\"assistant\",\"session_id\":\"legacy-fixture\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"$answer\"}]}}"
  printf '%s\n' '{"type":"result","subtype":"success","session_id":"legacy-fixture","is_error":false,"result":"done"}'
  printf '%s\n' "$tag" >> "$COSH_LEGACY_COMPLETION_FILE"
done
"#;

#[test]
fn raw_cli_legacy_bash_incremental_controls_and_provider_completion() {
    if !Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "(( BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4 ))",
        ])
        .status()
        .is_ok_and(|status| status.success())
    {
        eprintln!("SKIP: this PTY case requires system Bash 4.2 or 4.3");
        return;
    }
    let mut session = TerminalSession::spawn_for_shell_with_startup_env(
        "bash",
        "enhanced",
        100,
        &[
            ("bin/cosh-core", LEGACY_CORE),
            (
                "inputrc",
                "set editing-mode emacs\nset enable-bracketed-paste on\nset input-meta on\nset convert-meta off\nset output-meta on\n",
            ),
            (
                ".copilot-shell/config.toml",
                "[shell]\nadapter_default='cosh-core'\n",
            ),
        ],
        &[
            ("COSH_CORE_PATH", "$HOME/bin/cosh-core"),
            ("COSH_LEGACY_COMPLETION_FILE", "$HOME/provider-completed"),
            ("LC_ALL", "C"),
            ("LANG", "C"),
        ],
    );
    for (index, byte) in b"/help".iter().enumerate() {
        session.send(&[*byte]);
        let typed = format!("screen$ {}", String::from_utf8_lossy(&b"/help"[..=index]));
        session.wait_screen("incremental slash typing", |screen| {
            screen
                .contents()
                .lines()
                .last()
                .is_some_and(|line| line.starts_with(&typed))
        });
    }
    session.send(b"\r");
    session.wait_screen("slash result returns to prompt", |screen| {
        screen.contents().ends_with("screen$ ")
    });
    assert!(session.raw_output().contains("Slash commands"));
    assert!(!session.raw_output().contains("cannot find keymap"));
    session.send(b"/hep\x1b[Dl\x1b[F\r");
    session.wait_screen("edited slash result", |screen| {
        screen.contents().ends_with("screen$ ")
    });
    session.send(b"\x1b[A\r");
    session.wait_screen("recalled slash result", |screen| {
        screen.contents().ends_with("screen$ ")
    });
    assert_eq!(session.raw_output().matches("Slash commands").count(), 3);

    let receipt = session.home().join("provider-completed");
    session.send(b"Explain disk usage\r");
    session.wait_screen("English provider completion", |screen| {
        std::fs::read_to_string(&receipt).is_ok_and(|text| text == "english\n")
            && screen.contents().ends_with("screen$ ")
    });
    assert!(session.raw_output().contains("ENGLISH_PROVIDER_COMPLETED"));
    session.send("解释一下磁盘用量\r".as_bytes());
    session.wait_screen("Chinese provider completion", |screen| {
        std::fs::read_to_string(&receipt).is_ok_and(|text| text == "english\nchinese\n")
            && screen.contents().ends_with("screen$ ")
    });
    assert!(session.raw_output().contains("CHINESE_PROVIDER_COMPLETED"));
    assert!(!session.raw_output().contains("Agent timed out"));
    assert!(!session.raw_output().contains("bash: /help"));
    session.finish();
}
