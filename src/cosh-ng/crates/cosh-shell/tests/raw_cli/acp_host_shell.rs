use super::*;

/// Scripted ACP peer for the real-shell entry. It is a deterministic fixture,
/// not a real Agent: it answers the handshake, asks the Shell to own one final
/// action, then records every line it reads back. The terminal fact it records
/// comes from the real PTY; the fixture never supplies one.
fn acp_peer(home: &Path, label: &str, command: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = home.join(format!("peer-{label}.jsonl"));
    let script = home.join(format!("peer-{label}.sh"));
    let body = format!(
        r#"#!/bin/bash
log='{log}'
req_id() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
IFS= read -r init
printf '%s\n' "$init" >> "$log"
pty=$(printf '%s' "$init" | sed -n 's/.*"ptyInstance":"\([^"]*\)".*/\1/p')
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{{}}}}}}\n' "$(req_id "$init")"
IFS= read -r new
printf '%s\n' "$new" >> "$log"
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"acp-fixture-session"}}}}\n' "$(req_id "$new")"
IFS= read -r prompt
printf '%s\n' "$prompt" >> "$log"
pid=$(req_id "$prompt")
execute='{{"jsonrpc":"2.0","id":9001,"method":"_cosh/shell/v1/execute","params":{{"sessionId":"acp-fixture-session","toolCallId":"tool-1","command":"{command}","cwd":"/tmp","ptyInstance":"'"$pty"'","hookRequiresApproval":false}}}}'
printf '%s\n' "$execute"
while IFS= read -r -t 60 line; do
  printf '%s\n' "$line" >> "$log"
  case "$line" in
    *'"id":9001'*) break ;;
  esac
done
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$pid"
"#,
        log = log.display(),
        command = command,
    );
    write_executable(&script, &body);
    (script, log)
}

/// Runs the real binary against the scripted peer and returns the rendered
/// terminal plus the peer's own record of the wire.
fn run_acp_entry(label: &str, command: &str) -> (String, String, std::path::PathBuf) {
    let home = temp_shell_home(label);
    let home = std::fs::canonicalize(&home).expect("canonical home");
    let (script, log) = acp_peer(&home, label, command);
    let home_str = home.display().to_string();
    let script_str = script.display().to_string();
    let cd = format!("cd {home_str}\n");
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "acp",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_ACP_AGENT_PROGRAM", &script_str),
            ("COSH_SHELL_DEBUG", "1"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (cd.into_bytes(), Duration::from_millis(2_500)),
            (
                b"?? run the requested action\n".to_vec(),
                Duration::from_millis(2_500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(15_000)),
        ],
    );
    let wire = fs::read_to_string(&log).unwrap_or_default();
    (output, wire, home)
}

fn execute_result(wire: &str) -> String {
    wire.lines()
        .find(|line| line.contains("\"id\":9001") && line.contains("\"result\""))
        .unwrap_or_else(|| panic!("execute result; wire={wire}"))
        .to_string()
}

#[test]
fn raw_cli_acp_host_shell_execute_runs_in_the_real_pty_and_returns_the_terminal_fact() {
    let nonce = format!(
        "cosh-acp-pty-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let command = format!("echo {nonce}");
    let (output, wire, home) = run_acp_entry("acp-host-shell-ok", &command);
    let home_str = home.display().to_string();

    // The action really ran in the user's shell, not in a fixture stub.
    assert!(output.contains(&nonce), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");

    // One session only: a delivered terminal fact must not provoke a retry.
    assert_eq!(
        wire.matches("\"method\":\"initialize\"").count(),
        1,
        "{wire}"
    );
    // The session directory is the shell's own reported directory, never the
    // "<unknown>" placeholder and never the process launch directory.
    let new_session = wire
        .lines()
        .find(|line| line.contains("\"method\":\"session/new\""))
        .unwrap_or_else(|| panic!("session/new; wire={wire}"));
    assert!(
        new_session.contains(&format!("\"cwd\":\"{home_str}\"")),
        "{new_session}"
    );
    assert!(!new_session.contains("<unknown>"), "{new_session}");
    assert_ne!(home_str, env!("CARGO_MANIFEST_DIR"));

    // owned precedes the terminal fact, so a long review cannot look like an
    // undelivered request to the peer.
    let owned = wire
        .find("\"method\":\"_cosh/shell/v1/owned\"")
        .unwrap_or_else(|| panic!("owned notification; wire={wire}"));
    let result = execute_result(&wire);
    let result_at = wire.find(&result).expect("result position");
    assert!(owned < result_at, "{wire}");

    // The terminal fact carries real execution facts. The peer's cwd claim was
    // /tmp, so a truthful result proves the Shell never silently retargeted.
    assert!(
        result.contains(&format!("\"command\":\"echo {nonce}\"")),
        "{result}"
    );
    assert!(result.contains("\"status\":\"completed\""), "{result}");
    assert!(result.contains("\"exit_code\":0"), "{result}");
    assert!(
        result.contains(&format!("\"cwd\":\"{home_str}\"")),
        "{result}"
    );
    assert!(
        result.contains(&format!("\"end_cwd\":\"{home_str}\"")),
        "{result}"
    );
    assert!(!result.contains("\"cwd\":\"/tmp\""), "{result}");
    assert!(
        result.contains("\"output_ref\":\"terminal-output://"),
        "{result}"
    );
    assert!(result.contains("\"approval_id\":\"req-"), "{result}");
    assert!(result.contains("\"tool_use_id\":\"tool-1\""), "{result}");

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn raw_cli_acp_host_shell_reports_a_real_failure_without_claiming_success() {
    // A read-only action that really fails: ls reports 1 on macOS and 2 on
    // Linux, so the oracle is the non-zero fact and the failed status, not a
    // platform-specific number.
    let missing = format!(
        "/cosh-acp-missing-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let command = format!("ls {missing}");
    let (output, wire, home) = run_acp_entry("acp-host-shell-fail", &command);

    let result = execute_result(&wire);
    assert!(result.contains("\"status\":\"failed\""), "{result}");
    assert!(!result.contains("\"status\":\"completed\""), "{result}");
    assert!(!result.contains("\"exit_code\":0"), "{result}");
    assert!(result.contains("\"exit_code\":"), "{result}");
    assert_eq!(
        wire.matches("\"method\":\"initialize\"").count(),
        1,
        "{wire}"
    );
    assert!(output.contains("Bash tool sent to shell"), "{output}");

    let _ = fs::remove_dir_all(&home);
}
