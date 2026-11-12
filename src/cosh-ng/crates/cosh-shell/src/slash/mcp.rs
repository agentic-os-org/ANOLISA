use std::io::{Read, Write};
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use wait_timeout::ChildExt;

use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

fn text<'a>(i18n: &I18n, en: &'a str, zh: &'a str) -> &'a str {
    match i18n.language() {
        Language::EnUs => en,
        Language::ZhCn => zh,
    }
}

fn localized_status<'a>(status: &'a str, i18n: &I18n) -> &'a str {
    if i18n.language() == Language::EnUs {
        return status;
    }
    match status {
        "enabled" => "已启用",
        "disabled" => "已禁用",
        "connected" => "已连接",
        "disconnected" => "已断开",
        "pending" => "等待中",
        "error" | "failed" => "失败",
        "unknown" => "未知",
        _ => status,
    }
}

fn localized_action<'a>(action: &'a str, i18n: &I18n) -> &'a str {
    if i18n.language() == Language::EnUs {
        return action;
    }
    match action {
        "list" => "列出服务器",
        "connect" => "连接",
        "connected" => "已连接",
        "inspect" => "查看工具",
        "inspected" => "已查看工具",
        "refresh" => "刷新工具",
        "refreshed" => "已刷新工具",
        "disconnect" => "断开连接",
        "login" => "登录授权",
        "logout" => "删除授权凭据",
        _ => action,
    }
}

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
                format!(
                    "{}: {extra}",
                    text(&i18n, "Error: unexpected argument", "错误：多余的参数")
                ),
                format!("{}: /mcp {action} <server>", text(&i18n, "Usage", "用法")),
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
                text(
                    &i18n,
                    "OAuth login needs an interactive browser flow and cannot run inside the TUI.",
                    "OAuth 登录需要交互式浏览器授权，无法在 TUI 内完成。",
                )
                .to_string(),
                text(
                    &i18n,
                    "Run `cosh-core mcp login {server}` in a shell instead.",
                    "请在 Shell 中运行 `cosh-core mcp login {server}`。",
                )
                .replace("{server}", arg.unwrap_or("<server>")),
            ],
            None,
        ),
        "list" | "connect" | "inspect" | "refresh" | "disconnect" | "logout" => {
            if action != "list" && arg.is_none() {
                return render_notice_panel(
                    output,
                    title,
                    vec![
                        text(
                            &i18n,
                            "Error: /mcp {action} requires a server name.",
                            "错误：/mcp {action} 需要指定服务器名称。",
                        )
                        .replace("{action}", action),
                        format!("{}: /mcp {action} <server>", text(&i18n, "Usage", "用法")),
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
            let succeeded =
                run_mcp_subprocess(&cosh_core.program, &cmd_args, title, output, &i18n)?;
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
                text(
                    &i18n,
                    "Usage: /mcp <command> [server]",
                    "用法: /mcp <command> [server]",
                )
                .to_string(),
                String::new(),
                text(&i18n, "Commands:", "命令：").to_string(),
                text(
                    &i18n,
                    "  list         List configured MCP servers",
                    "  list         列出已配置的 MCP 服务器",
                )
                .to_string(),
                text(
                    &i18n,
                    "  connect      Connect to an MCP server",
                    "  connect      连接 MCP 服务器",
                )
                .to_string(),
                text(
                    &i18n,
                    "  inspect      Inspect an MCP server's tools",
                    "  inspect      查看 MCP 服务器的工具",
                )
                .to_string(),
                text(
                    &i18n,
                    "  refresh      Refresh an MCP server's tool list",
                    "  refresh      刷新 MCP 服务器的工具列表",
                )
                .to_string(),
                text(
                    &i18n,
                    "  disconnect   Disconnect an MCP server",
                    "  disconnect   断开 MCP 服务器",
                )
                .to_string(),
                text(
                    &i18n,
                    "  login        Authorize an MCP server (OAuth, runs in a shell)",
                    "  login        授权 MCP 服务器（OAuth，需在 Shell 中运行）",
                )
                .to_string(),
                text(
                    &i18n,
                    "  logout       Remove saved OAuth credentials",
                    "  logout       删除保存的 OAuth 凭据",
                )
                .to_string(),
            ],
            None,
        ),
        _ => render_notice_panel(
            output,
            title,
            vec![
                format!(
                    "{}: {action}",
                    text(&i18n, "Unknown subcommand", "未知子命令")
                ),
                text(
                    &i18n,
                    "Run /mcp help for usage information.",
                    "运行 /mcp help 查看用法。",
                )
                .to_string(),
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
    i18n: &I18n,
) -> std::io::Result<bool> {
    match capture_mcp_cli(program, args, i18n) {
        Ok(capture) => {
            let stdout = String::from_utf8_lossy(&capture.stdout);
            let stderr = String::from_utf8_lossy(&capture.stderr);

            if capture.success {
                let body = mcp_success_body(&stdout, &stderr, i18n, args);
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
                } else if let Some(extracted) = extract_json_message_or_error(&stdout, i18n) {
                    vec![extracted]
                } else if let Some(first_line) = stdout.lines().next() {
                    vec![first_line.to_string()]
                } else {
                    vec![format!(
                        "{}: {}",
                        text(i18n, "Command failed", "命令失败"),
                        capture.status_display
                    )]
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
fn mcp_success_body(stdout: &str, stderr: &str, i18n: &I18n, args: &[&str]) -> Vec<String> {
    if let ["mcp", "logout", server] = args {
        let original = format!("OAuth credentials removed for MCP server '{server}'.");
        let confirmation = text(
            i18n,
            "OAuth credentials removed for MCP server '{server}'.",
            "已删除 MCP 服务器 '{server}' 的 OAuth 凭据。",
        )
        .replace("{server}", server);
        let mut lines = vec![confirmation];
        lines.extend(format_mcp_output(stdout, i18n));
        lines.extend(
            stderr
                .lines()
                .filter(|line| !line.is_empty() && *line != original)
                .map(str::to_string),
        );
        return lines;
    }
    if stdout.trim().is_empty() && !stderr.trim().is_empty() {
        return stderr
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect();
    }
    format_mcp_output(stdout, i18n)
}

struct McpCliCapture {
    success: bool,
    status_display: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn capture_mcp_cli(program: &str, args: &[&str], i18n: &I18n) -> Result<McpCliCapture, String> {
    capture_mcp_cli_with_timeout(program, args, MCP_CLI_TIMEOUT, i18n)
}

fn capture_mcp_cli_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
    i18n: &I18n,
) -> Result<McpCliCapture, String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut command = crate::adapter::command_with_trusted_startup_path(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "{}: {error}",
            text(i18n, "Failed to run cosh-core", "无法运行 cosh-core")
        )
    })?;
    let process_group = child.id();
    let stdout_receiver = drain_pipe(child.stdout.take());
    let stderr_receiver = drain_pipe(child.stderr.take());

    let status = match child.wait_timeout(timeout).map_err(|error| {
        format!(
            "{}: {error}",
            text(i18n, "Failed to wait for cosh-core", "等待 cosh-core 失败")
        )
    })? {
        Some(status) => status,
        None => {
            kill_mcp_process_group(process_group);
            // Child::kill is the cross-platform fallback: the process-group
            // SIGKILL above is unix-only, and wait() would block forever on
            // other platforms if the child were left running.
            let _ = child.kill();
            let _ = child.wait();
            return Err(text(
                i18n,
                "cosh-core mcp timed out after {seconds}s.",
                "cosh-core mcp 在 {seconds} 秒后超时。",
            )
            .replace("{seconds}", &timeout.as_secs().to_string()));
        }
    };

    let collect = |receiver: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        receiver
            .recv_timeout(remaining)
            .map_err(|_| {
                kill_mcp_process_group(process_group);
                text(
                    i18n,
                    "cosh-core mcp output did not close before timeout.",
                    "cosh-core mcp 输出未在超时前关闭。",
                )
                .to_string()
            })?
            .map_err(|error| {
                format!(
                    "{}: {error}",
                    text(
                        i18n,
                        "Failed to read cosh-core output",
                        "读取 cosh-core 输出失败"
                    )
                )
            })
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

fn format_mcp_output(stdout: &str, i18n: &I18n) -> Vec<String> {
    // Try to parse as JSON and format nicely
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        format_mcp_json(&json, i18n)
    } else {
        // Fall back to raw output lines
        stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect()
    }
}

fn extract_json_message_or_error(stdout: &str, i18n: &I18n) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    extract_message_or_error(&json, i18n)
}

fn extract_message_or_error(json: &serde_json::Value, i18n: &I18n) -> Option<String> {
    // Prioritize top-level message or error fields
    if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
        return Some(message.to_string());
    }
    if let Some(error) = json.get("error").and_then(|v| v.as_str()) {
        return Some(format!("{}: {error}", text(i18n, "Error", "错误")));
    }
    if let Some(error_obj) = json.get("error").and_then(|v| v.as_object()) {
        if let Some(msg) = error_obj.get("message").and_then(|v| v.as_str()) {
            return Some(format!("{}: {msg}", text(i18n, "Error", "错误")));
        }
    }
    None
}

fn format_mcp_json(json: &serde_json::Value, i18n: &I18n) -> Vec<String> {
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
            lines.push(
                text(i18n, "No MCP servers configured.", "尚未配置 MCP 服务器。").to_string(),
            );
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
            let status = localized_status(status, i18n);
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
                    lines.push(format!(
                        "    {} ({}):",
                        text(i18n, "Tools", "工具"),
                        tools.len()
                    ));
                    for tool in tools.iter().take(20) {
                        let tool_name = tool
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        lines.push(format!("      - {tool_name}"));
                    }
                    if tools.len() > 20 {
                        lines.push(format!(
                            "      {}",
                            text(i18n, "... and {count} more", "……另有 {count} 项")
                                .replace("{count}", &(tools.len() - 20).to_string())
                        ));
                    }
                }
            }
        }
    } else if let (Some(server), Some(true), Some(removed)) = (
        json.get("server").and_then(serde_json::Value::as_str),
        json.get("disabled").and_then(serde_json::Value::as_bool),
        json.get("credentials_removed")
            .and_then(serde_json::Value::as_bool),
    ) {
        lines.push(
            text(
                i18n,
                "MCP server '{server}' disconnected.",
                "MCP 服务器 '{server}' 已断开。",
            )
            .replace("{server}", server),
        );
        lines.push(
            if removed {
                text(
                    i18n,
                    "Saved OAuth credentials removed.",
                    "已删除保存的 OAuth 凭据。",
                )
            } else {
                text(
                    i18n,
                    "No saved OAuth credentials to remove.",
                    "无已保存的 OAuth 凭据需要删除。",
                )
            }
            .to_string(),
        );
    } else if let Some(action) = json.get("action").and_then(|v| v.as_str()) {
        lines.push(format!(
            "{}: {}",
            text(i18n, "Action", "操作"),
            localized_action(action, i18n)
        ));
        if let Some(server) = json.get("server").and_then(|v| v.as_str()) {
            lines.push(format!("{}: {server}", text(i18n, "Server", "服务器")));
        }
        if let Some(tools) = json.get("tools").and_then(|v| v.as_array()) {
            lines.push(format!(
                "{} ({}):",
                text(i18n, "Tools", "工具"),
                tools.len()
            ));
            for tool in tools.iter().take(20) {
                let name = tool
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                lines.push(format!("  - {name}"));
            }
            if tools.len() > 20 {
                lines.push(format!(
                    "  {}",
                    text(i18n, "... and {count} more", "……另有 {count} 项")
                        .replace("{count}", &(tools.len() - 20).to_string())
                ));
            }
        }
        // Include any message field
        if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
            lines.push(message.to_string());
        }
    } else if let Some(fallback) = extract_message_or_error(json, i18n) {
        // Structure doesn't match expected patterns, but has message/error
        lines.push(fallback);
    } else {
        // Generic JSON formatting as last resort
        lines.push(serde_json::to_string_pretty(json).unwrap_or_else(|_| json.to_string()));
    }

    lines
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
