use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use wait_timeout::ChildExt;

use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

// Bounded so a stuck `cosh-core mcp` call (e.g. an OAuth login waiting on a
// browser) cannot block the slash rendering path indefinitely.
const MCP_CLI_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_CLI_MAX_OUTPUT: usize = 1024 * 1024;

pub(super) fn render_mcp_command<W: Write>(
    sub: Option<&str>,
    arg: Option<&str>,
    extra: Option<&str>,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let title = i18n.t(MessageId::SlashMcpTitle);

    let action = sub.unwrap_or("list");

    if let Some(extra) = extra {
        return render_notice_panel(
            output,
            title,
            vec![
                format!("Error: unexpected argument: {extra}"),
                format!("Usage: /mcp {action} <server>"),
            ],
            None,
        );
    }

    match action {
        // OAuth login prints an authorization URL and waits for the browser
        // callback, but the synchronous slash path only shows subprocess
        // output after exit — the URL would never surface before the timeout.
        "login" => render_notice_panel(
            output,
            title,
            vec![
                "OAuth login needs an interactive browser flow and cannot run inside the TUI."
                    .to_string(),
                format!(
                    "Run `cosh-core mcp login {}` in a shell instead.",
                    arg.unwrap_or("<server>")
                ),
            ],
            None,
        ),
        "list" | "connect" | "inspect" | "refresh" | "disconnect" | "logout" => {
            if action != "list" && arg.is_none() {
                return render_notice_panel(
                    output,
                    title,
                    vec![
                        format!("Error: /mcp {action} requires a server name."),
                        format!("Usage: /mcp {action} <server>"),
                    ],
                    None,
                );
            }
            let AdapterInstance::CoshCore(cosh_core) = adapter else {
                return render_notice_panel(
                    output,
                    title,
                    vec![i18n.t(MessageId::SlashRegistryUnavailable).to_string()],
                    None,
                );
            };
            let mut cmd_args = vec!["mcp", action];
            if let Some(server) = arg {
                cmd_args.push(server);
            }
            let succeeded = run_mcp_subprocess(&cosh_core.program, &cmd_args, title, output)?;
            // The subprocess only mutates on-disk state; the live core keeps
            // serving its startup snapshot (tools, connections, credentials)
            // until an extension snapshot rebuild picks the changes up.
            if succeeded && matches!(action, "connect" | "disconnect" | "refresh" | "logout") {
                cosh_core.note_mcp_mutation();
            }
            Ok(())
        }
        "help" | "--help" | "-h" => render_notice_panel(
            output,
            title,
            vec![
                "Usage: /mcp <command> [server]".to_string(),
                String::new(),
                "Commands:".to_string(),
                "  list         List configured MCP servers".to_string(),
                "  connect      Connect to an MCP server".to_string(),
                "  inspect      Inspect an MCP server's tools".to_string(),
                "  refresh      Refresh an MCP server's tool list".to_string(),
                "  disconnect   Disconnect an MCP server".to_string(),
                "  login        Authorize an MCP server (OAuth, runs in a shell)".to_string(),
                "  logout       Remove saved OAuth credentials".to_string(),
            ],
            None,
        ),
        _ => render_notice_panel(
            output,
            title,
            vec![
                format!("Unknown subcommand: {action}"),
                "Run /mcp help for usage information.".to_string(),
            ],
            None,
        ),
    }
}

fn run_mcp_subprocess<W: Write>(
    program: &str,
    args: &[&str],
    title: &str,
    output: &mut W,
) -> std::io::Result<bool> {
    match capture_mcp_cli(program, args) {
        Ok(capture) => {
            let stdout = String::from_utf8_lossy(&capture.stdout);
            let stderr = String::from_utf8_lossy(&capture.stderr);

            if capture.success {
                let body = mcp_success_body(&stdout, &stderr);
                render_notice_panel(output, title, body, None)?;
                Ok(true)
            } else {
                // On failure, prioritize stderr, then extract message/error from stdout JSON
                let error_lines: Vec<String> = if !stderr.trim().is_empty() {
                    stderr
                        .lines()
                        .filter(|line| !line.is_empty())
                        .map(|line| line.to_string())
                        .collect()
                } else if let Some(extracted) = extract_json_message_or_error(&stdout) {
                    vec![extracted]
                } else if let Some(first_line) = stdout.lines().next() {
                    vec![first_line.to_string()]
                } else {
                    vec![format!("Command failed: {}", capture.status_display)]
                };
                render_notice_panel(output, title, error_lines, None)?;
                Ok(false)
            }
        }
        Err(error) => {
            render_notice_panel(output, title, vec![error], None)?;
            Ok(false)
        }
    }
}

// cosh-core writes some human-readable confirmations (e.g. logout) to stderr
// while stdout stays empty; without the fallback the success panel would be
// blank.
fn mcp_success_body(stdout: &str, stderr: &str) -> Vec<String> {
    if stdout.trim().is_empty() && !stderr.trim().is_empty() {
        return stderr
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect();
    }
    format_mcp_output(stdout)
}

struct McpCliCapture {
    success: bool,
    status_display: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn capture_mcp_cli(program: &str, args: &[&str]) -> Result<McpCliCapture, String> {
    capture_mcp_cli_with_timeout(program, args, MCP_CLI_TIMEOUT)
}

fn capture_mcp_cli_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<McpCliCapture, String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Failed to run cosh-core: {error}"))?;
    let process_group = child.id();
    let stdout_receiver = drain_pipe(child.stdout.take());
    let stderr_receiver = drain_pipe(child.stderr.take());

    let status = match child
        .wait_timeout(timeout)
        .map_err(|error| format!("Failed to wait for cosh-core: {error}"))?
    {
        Some(status) => status,
        None => {
            kill_mcp_process_group(process_group);
            // Child::kill is the cross-platform fallback: the process-group
            // SIGKILL above is unix-only, and wait() would block forever on
            // other platforms if the child were left running.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "cosh-core mcp timed out after {}s.",
                timeout.as_secs()
            ));
        }
    };

    let collect = |receiver: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        receiver
            .recv_timeout(remaining)
            .map_err(|_| {
                kill_mcp_process_group(process_group);
                "cosh-core mcp output did not close before timeout.".to_string()
            })?
            .map_err(|error| format!("Failed to read cosh-core output: {error}"))
    };
    let stdout = collect(stdout_receiver)?;
    let stderr = collect(stderr_receiver)?;

    Ok(McpCliCapture {
        success: status.success(),
        status_display: status.to_string(),
        stdout,
        stderr,
    })
}

fn drain_pipe<R: Read + Send + 'static>(
    pipe: Option<R>,
) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = match pipe {
            Some(source) => {
                let mut bytes = Vec::new();
                source
                    .take(MCP_CLI_MAX_OUTPUT as u64)
                    .read_to_end(&mut bytes)
                    .map(|_| bytes)
            }
            None => Ok(Vec::new()),
        };
        let _ = sender.send(result);
    });
    receiver
}

#[cfg(unix)]
fn kill_mcp_process_group(process_group: u32) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    let _ = killpg(Pid::from_raw(process_group as i32), Signal::SIGKILL);
}

#[cfg(not(unix))]
fn kill_mcp_process_group(_process_group: u32) {}

fn format_mcp_output(stdout: &str) -> Vec<String> {
    // Try to parse as JSON and format nicely
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        format_mcp_json(&json)
    } else {
        // Fall back to raw output lines
        stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect()
    }
}

fn extract_json_message_or_error(stdout: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    extract_message_or_error(&json)
}

fn extract_message_or_error(json: &serde_json::Value) -> Option<String> {
    // Prioritize top-level message or error fields
    if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
        return Some(message.to_string());
    }
    if let Some(error) = json.get("error").and_then(|v| v.as_str()) {
        return Some(format!("Error: {error}"));
    }
    if let Some(error_obj) = json.get("error").and_then(|v| v.as_object()) {
        if let Some(msg) = error_obj.get("message").and_then(|v| v.as_str()) {
            return Some(format!("Error: {msg}"));
        }
    }
    None
}

fn format_mcp_json(json: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();

    // Try structured formats first
    // Case 1: {"servers": [...]} wrapper
    // Case 2: Direct array [{...}, {...}]
    let servers_array = json
        .get("servers")
        .and_then(|v| v.as_array())
        .or_else(|| json.as_array());

    if let Some(servers) = servers_array {
        if servers.is_empty() {
            lines.push("No MCP servers configured.".to_string());
            return lines;
        }
        for server in servers {
            let name = server
                .get("server")
                .or_else(|| server.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let status = server
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| match server.get("enabled").and_then(|v| v.as_bool()) {
                    Some(true) => "enabled",
                    Some(false) => "disabled",
                    None => "unknown",
                });
            let transport = server
                .get("transport")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if transport.is_empty() {
                lines.push(format!("  {name}  [{status}]"));
            } else {
                lines.push(format!("  {name}  [{status}]  {transport}"));
            }

            if let Some(tools) = server.get("tools").and_then(|v| v.as_array()) {
                if !tools.is_empty() {
                    lines.push(format!("    Tools ({}):", tools.len()));
                    for tool in tools.iter().take(20) {
                        let tool_name = tool
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        lines.push(format!("      - {tool_name}"));
                    }
                    if tools.len() > 20 {
                        lines.push(format!("      ... and {} more", tools.len() - 20));
                    }
                }
            }
        }
    } else if let Some(action) = json.get("action").and_then(|v| v.as_str()) {
        lines.push(format!("Action: {action}"));
        if let Some(server) = json.get("server").and_then(|v| v.as_str()) {
            lines.push(format!("Server: {server}"));
        }
        if let Some(tools) = json.get("tools").and_then(|v| v.as_array()) {
            lines.push(format!("Tools ({}):", tools.len()));
            for tool in tools.iter().take(20) {
                let name = tool
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                lines.push(format!("  - {name}"));
            }
            if tools.len() > 20 {
                lines.push(format!("  ... and {} more", tools.len() - 20));
            }
        }
        // Include any message field
        if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
            lines.push(message.to_string());
        }
    } else if let Some(fallback) = extract_message_or_error(json) {
        // Structure doesn't match expected patterns, but has message/error
        lines.push(fallback);
    } else {
        // Generic JSON formatting as last resort
        lines.push(serde_json::to_string_pretty(json).unwrap_or_else(|_| json.to_string()));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::{
        extract_json_message_or_error, format_mcp_json, format_mcp_output, mcp_success_body,
    };

    #[test]
    fn success_panel_falls_back_to_stderr_when_stdout_is_empty() {
        let confirmation = "OAuth credentials removed for MCP server 'remote'.";
        assert_eq!(
            mcp_success_body("", &format!("{confirmation}\n")),
            vec![confirmation.to_string()]
        );
        assert_eq!(
            mcp_success_body("{\"message\":\"connected\"}", "progress noise"),
            vec!["connected".to_string()]
        );
        assert!(mcp_success_body("", "").is_empty());
    }

    #[test]
    fn server_status_falls_back_to_enabled_then_unknown() {
        let json = serde_json::json!({"servers": [
            {"server": "a", "status": "pending"},
            {"server": "b", "enabled": true},
            {"server": "c", "enabled": false},
            {"server": "d"},
        ]});
        let lines = format_mcp_json(&json);
        assert!(
            lines[0].contains("a") && lines[0].contains("[pending]"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("b") && lines[1].contains("[enabled]"),
            "{lines:?}"
        );
        assert!(
            lines[2].contains("c") && lines[2].contains("[disabled]"),
            "{lines:?}"
        );
        assert!(
            lines[3].contains("d") && lines[3].contains("[unknown]"),
            "{lines:?}"
        );
    }

    #[test]
    fn action_tool_list_truncation_matches_servers_branch() {
        let tools: Vec<serde_json::Value> = (0..23)
            .map(|index| serde_json::json!({"name": format!("tool-{index}")}))
            .collect();
        let action = serde_json::json!({"action": "inspect", "tools": tools});
        let action_lines = format_mcp_json(&action);
        assert!(
            action_lines.contains(&"  ... and 3 more".to_string()),
            "{action_lines:?}"
        );

        let servers = serde_json::json!({"servers": [
            {"server": "a", "status": "enabled", "tools": tools},
        ]});
        let server_lines = format_mcp_json(&servers);
        assert!(
            server_lines.contains(&"      ... and 3 more".to_string()),
            "{server_lines:?}"
        );
    }

    #[test]
    fn message_and_error_extraction_share_one_path() {
        let message = serde_json::json!({"message": "connected"});
        assert_eq!(format_mcp_json(&message), vec!["connected".to_string()]);

        let error_string = serde_json::json!({"error": "not found"});
        assert_eq!(
            format_mcp_json(&error_string),
            vec!["Error: not found".to_string()]
        );

        let error_object = serde_json::json!({"error": {"message": "denied"}});
        assert_eq!(
            format_mcp_json(&error_object),
            vec!["Error: denied".to_string()]
        );

        assert_eq!(
            extract_json_message_or_error(r#"{"error": {"message": "denied"}}"#),
            Some("Error: denied".to_string())
        );
        assert_eq!(extract_json_message_or_error("not json"), None);
    }

    #[test]
    fn non_json_output_falls_back_to_raw_lines() {
        assert_eq!(
            format_mcp_output("first\n\nsecond\n"),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn transport_absent_leaves_no_trailing_whitespace() {
        let json = serde_json::json!({"servers": [{"server": "a", "enabled": true}]});
        let lines = format_mcp_json(&json);
        assert_eq!(lines[0], lines[0].trim_end(), "{lines:?}");
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_timeout_kills_slow_cosh_core() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let root = std::env::temp_dir().join(format!("mcp-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let slow = root.join("slow-cosh-core");
        std::fs::write(&slow, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o700)).unwrap();

        let started = std::time::Instant::now();
        let result = super::capture_mcp_cli_with_timeout(
            slow.to_str().unwrap(),
            &["mcp", "list"],
            Duration::from_millis(50),
        );
        let Err(message) = result else {
            panic!("slow cosh-core must time out");
        };
        assert!(message.contains("timed out"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(2));

        assert!(super::capture_mcp_cli("/definitely/missing/cosh-core", &[]).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
