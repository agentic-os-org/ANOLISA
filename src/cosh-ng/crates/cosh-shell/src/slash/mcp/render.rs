//! Rendering helpers for the `/mcp` slash panels (split from `slash/mcp.rs` to
//! keep the dispatch module under the layout line threshold).

use crate::runtime::prelude::*;

pub(super) fn text<'a>(i18n: &I18n, en: &'a str, zh: &'a str) -> &'a str {
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
        "show" => "查看命令与信任",
        "trust" => "信任启动",
        "untrust" => "撤销启动信任",
        _ => action,
    }
}

pub(super) fn format_mcp_output(stdout: &str, i18n: &I18n) -> Vec<String> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        format_mcp_json(&json, i18n)
    } else {
        stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect()
    }
}

pub(super) fn extract_json_message_or_error(stdout: &str, i18n: &I18n) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    extract_message_or_error(&json, i18n)
}

fn extract_message_or_error(json: &serde_json::Value, i18n: &I18n) -> Option<String> {
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

pub(super) fn format_mcp_json(json: &serde_json::Value, i18n: &I18n) -> Vec<String> {
    let mut lines = Vec::new();

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
            let needs_trust = transport == "stdio"
                && server.get("trusted").and_then(|v| v.as_bool()) == Some(false);
            let trust_hint = if needs_trust {
                format!(
                    "  {}",
                    text(i18n, "needs trust (/mcp trust)", "待信任 (/mcp trust)")
                )
            } else {
                String::new()
            };
            if transport.is_empty() {
                lines.push(format!("  {name}  [{status}]{trust_hint}"));
            } else {
                lines.push(format!("  {name}  [{status}]  {transport}{trust_hint}"));
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
        if let Some(command) = json.get("command").and_then(|v| v.as_str()) {
            lines.push(format!("{}: {command}", text(i18n, "Command", "命令")));
        }
        if let Some(args) = json.get("args").and_then(|v| v.as_array()) {
            let joined = args
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!("{}: {joined}", text(i18n, "Args", "参数")));
        }
        if let Some(trusted) = json.get("trusted").and_then(|v| v.as_bool()) {
            let label = if trusted {
                text(i18n, "yes", "是")
            } else {
                text(i18n, "no", "否")
            };
            lines.push(format!("{}: {label}", text(i18n, "Trusted", "已信任")));
        }
        if let Some(fingerprint) = json.get("fingerprint").and_then(|v| v.as_str()) {
            lines.push(format!(
                "{}: {fingerprint}",
                text(i18n, "Fingerprint", "指纹")
            ));
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
        if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
            lines.push(message.to_string());
        }
        if action == "trust" || action == "untrust" {
            lines.push(
                text(
                    i18n,
                    "Applies to future startups; the current session picks it up on the next rebuild. Restart cosh if you edited the config or the rebuild fails.",
                    "应用于后续启动；当前会话在下次重建时生效。若修改了配置或重建失败，请重启 cosh。",
                )
                .to_string(),
            );
        }
    } else if let Some(fallback) = extract_message_or_error(json, i18n) {
        lines.push(fallback);
    } else {
        lines.push(serde_json::to_string_pretty(json).unwrap_or_else(|_| json.to_string()));
    }

    lines
}
