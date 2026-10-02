mod parser;

use serde_json::{json, Value};

use self::parser::ExtensionCommand;
use crate::adapter::RegistryQueryError;
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

pub(super) fn render_extensions_command<W: Write>(
    args: &str,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        return render_notice_panel(
            output,
            i18n.t(MessageId::SlashExtensionsTitle),
            vec![i18n.t(MessageId::SlashRegistryUnavailable).to_string()],
            None,
        );
    };
    let command = match parser::parse(args) {
        Ok(command) => command,
        Err(error) => return render(output, &i18n, vec![localized_error(&i18n, &error)]),
    };

    let result = match command {
        ExtensionCommand::List => query(cosh_core, "list", Value::Null, |data| {
            format_extensions_list(data, &i18n)
        }),
        ExtensionCommand::Info { name } => {
            query(cosh_core, "info", json!({"name": name}), |data| {
                format_extension_detail(data, &i18n)
            })
        }
        ExtensionCommand::Doctor { name } => {
            query(cosh_core, "doctor", json!({"name": name}), |data| {
                format_doctor(data, &i18n)
            })
        }
        ExtensionCommand::New { path, template } => query(
            cosh_core,
            "new",
            json!({"path": path, "template": template}),
            |data| {
                vec![tr(
                    &i18n,
                    &format!("Created extension scaffold at {}.", field(data, "path")),
                    &format!("已在 {} 创建扩展脚手架。", field(data, "path")),
                )]
            },
        ),
        ExtensionCommand::Install { source, git_ref } => {
            let is_git = source.starts_with("https://");
            if git_ref.is_some() && !is_git {
                Err(tr(
                    &i18n,
                    "--ref is only valid for HTTPS Git sources.",
                    "--ref 仅适用于 HTTPS Git 源。",
                ))
            } else {
                preflight(
                    cosh_core,
                    "install-preflight",
                    json!({
                        "source": source,
                        "source_kind": if is_git { "git-https" } else { "path-copy" },
                        "ref": git_ref,
                    }),
                    &i18n,
                )
            }
        }
        ExtensionCommand::Link { source } => preflight(
            cosh_core,
            "link-preflight",
            json!({"source": source}),
            &i18n,
        ),
        ExtensionCommand::Update { name } => {
            preflight(cosh_core, "update-preflight", json!({"name": name}), &i18n)
        }
        ExtensionCommand::UpdateAll => {
            update_all_or_query_result(cosh_core).map(|data| format_update_all(&data, &i18n))
        }
        ExtensionCommand::Uninstall { name } => {
            query(cosh_core, "uninstall", json!({"name": name}), |data| {
                format_mutation(data, &i18n)
            })
        }
        ExtensionCommand::Enable { name } => {
            query(cosh_core, "enable", json!({"name": name}), |data| {
                format_mutation(data, &i18n)
            })
        }
        ExtensionCommand::Disable { name } => {
            query(cosh_core, "disable", json!({"name": name}), |data| {
                format_mutation(data, &i18n)
            })
        }
        ExtensionCommand::SelectSource { name, source } => query(
            cosh_core,
            "select-source",
            json!({"name": name, "source": source}),
            |data| format_mutation(data, &i18n),
        ),
        ExtensionCommand::SettingsList { name, scope } => query(
            cosh_core,
            "settings-list",
            json!({"name": name, "scope": scope}),
            |data| format_settings_list(data, &i18n),
        ),
        ExtensionCommand::SettingsGet { name, key, scope } => query(
            cosh_core,
            "settings-get",
            json!({"name": name, "key": key, "scope": scope}),
            |data| format_setting(data, &i18n),
        ),
        ExtensionCommand::SettingsSet {
            name,
            key,
            value,
            scope,
        } => query(
            cosh_core,
            "settings-set",
            json!({"name": name, "key": key, "value": value, "scope": scope}),
            |data| {
                let setting = data.get("setting").unwrap_or(&Value::Null);
                let mut lines = format_setting(setting, &i18n);
                lines.push(tr(
                    &i18n,
                    &format!("Activation: {}", field(data, "activation")),
                    &format!("生效边界：{}", field(data, "activation")),
                ));
                lines
            },
        ),
        ExtensionCommand::SettingsUnset { name, key, scope } => query(
            cosh_core,
            "settings-unset",
            json!({"name": name, "key": key, "scope": scope}),
            |data| {
                let setting = data.get("setting").unwrap_or(&Value::Null);
                format_setting(setting, &i18n)
            },
        ),
        ExtensionCommand::Reload => query(cosh_core, "reload", Value::Null, |data| {
            vec![tr(
                &i18n,
                &format!(
                    "Extensions refreshed; activation is {} (generation {}).",
                    field(data, "activation"),
                    field(data, "generation")
                ),
                &format!(
                    "扩展目录已刷新；生效边界为 {}（generation {}）。",
                    field(data, "activation"),
                    field(data, "generation")
                ),
            )]
        }),
        ExtensionCommand::Operation { operation_id } => {
            operation_or_result(cosh_core, &operation_id, &i18n)
        }
        ExtensionCommand::Consent { operation_id } => consent(cosh_core, &operation_id, &i18n),
        ExtensionCommand::Cancel { operation_id } => query(
            cosh_core,
            "cancel",
            json!({"operation_id": operation_id}),
            |_| vec![tr(&i18n, "Operation cancelled.", "操作已取消。")],
        ),
        ExtensionCommand::Help => Ok(help(&i18n)),
    };

    render(
        output,
        &i18n,
        result.unwrap_or_else(|error| vec![localized_error(&i18n, &error)]),
    )
}

fn query<F>(
    adapter: &crate::adapter::CoshCoreAdapter,
    action: &str,
    params: Value,
    format: F,
) -> Result<Vec<String>, String>
where
    F: FnOnce(&Value) -> Vec<String>,
{
    adapter
        .registry_query("extensions", action, params)
        .map(|data| format(&data))
}

fn preflight(
    adapter: &crate::adapter::CoshCoreAdapter,
    action: &str,
    params: Value,
    i18n: &I18n,
) -> Result<Vec<String>, String> {
    let data = adapter.registry_query("extensions", action, params)?;
    if data
        .get("consent_required")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        Ok(format_preflight(&data, i18n))
    } else {
        let operation_id = string_field(&data, "operation_id")?;
        let fingerprint = string_field(&data, "capability_fingerprint")?;
        let committed = commit_or_query_result(adapter, operation_id, fingerprint)?;
        Ok(format_mutation(&committed, i18n))
    }
}

fn consent(
    adapter: &crate::adapter::CoshCoreAdapter,
    operation_id: &str,
    i18n: &I18n,
) -> Result<Vec<String>, String> {
    let operation = adapter.registry_query(
        "extensions",
        "operation",
        json!({"operation_id": operation_id}),
    )?;
    let authoritative_id = string_field(&operation, "operation_id")?;
    if authoritative_id != operation_id {
        return Err("operation identity mismatch".to_string());
    }
    let fingerprint = string_field(&operation, "capability_fingerprint")?;
    let committed = commit_or_query_result(adapter, operation_id, fingerprint)?;
    Ok(format_mutation(&committed, i18n))
}

fn operation_or_result(
    adapter: &crate::adapter::CoshCoreAdapter,
    operation_id: &str,
    i18n: &I18n,
) -> Result<Vec<String>, String> {
    match adapter.registry_query(
        "extensions",
        "operation",
        json!({"operation_id": operation_id}),
    ) {
        Ok(operation) => Ok(format_preflight(&operation, i18n)),
        Err(error) if error.contains("extension_operation_not_found") => adapter
            .registry_query(
                "extensions",
                "result",
                json!({"operation_id": operation_id}),
            )
            .map(|result| format_operation_result(&result, i18n)),
        Err(error) => Err(error),
    }
}

fn format_operation_result(data: &Value, i18n: &I18n) -> Vec<String> {
    if data.get("action").and_then(Value::as_str) == Some("update-all") {
        format_update_all(data, i18n)
    } else {
        format_mutation(data, i18n)
    }
}

fn commit_or_query_result(
    adapter: &crate::adapter::CoshCoreAdapter,
    operation_id: &str,
    fingerprint: &str,
) -> Result<Value, String> {
    match adapter.registry_query_classified(
        "extensions",
        "commit",
        json!({"operation_id": operation_id, "fingerprint": fingerprint}),
    ) {
        Ok(result) => Ok(result),
        Err(RegistryQueryError::Response { message, .. }) => Err(message),
        Err(RegistryQueryError::Transport(commit_error)) => adapter
            .registry_query(
                "extensions",
                "result",
                json!({"operation_id": operation_id}),
            )
            .map_err(|status_error| {
                format!(
                    "commit status is unknown after transport failure: {commit_error}; result query failed: {status_error}"
                )
            }),
    }
}

fn update_all_or_query_result(adapter: &crate::adapter::CoshCoreAdapter) -> Result<Value, String> {
    let prepared = adapter.registry_query("extensions", "update-all-preflight", Value::Null)?;
    let operation_id = string_field(&prepared, "operation_id")?;
    match adapter.registry_query_classified(
        "extensions",
        "update-all-commit",
        json!({"operation_id": operation_id}),
    ) {
        Ok(result) => Ok(result),
        Err(RegistryQueryError::Response { message, .. }) => Err(message),
        Err(RegistryQueryError::Transport(commit_error)) => adapter
            .registry_query(
                "extensions",
                "result",
                json!({"operation_id": operation_id}),
            )
            .map_err(|status_error| {
                format!(
                    "update-all status is unknown after transport failure: {commit_error}; result query failed: {status_error}"
                )
            }),
    }
}

fn render<W: Write>(output: &mut W, i18n: &I18n, body: Vec<String>) -> std::io::Result<()> {
    render_notice_panel(output, i18n.t(MessageId::SlashExtensionsTitle), body, None)
}

fn format_extensions_list(data: &Value, i18n: &I18n) -> Vec<String> {
    let Some(items) = data.as_array() else {
        return vec![i18n.t(MessageId::SlashExtensionsEmptyBody).to_string()];
    };
    if items.is_empty() {
        return vec![i18n.t(MessageId::SlashExtensionsEmptyBody).to_string()];
    }
    items
        .iter()
        .filter_map(|extension| {
            let name = extension.get("name")?.as_str()?;
            let version = extension
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let desired = field(extension, "desired_state");
            let effective = field(extension, "effective_state");
            Some(format!(
                "  • {name} v{version} (desired={desired}, effective={effective})"
            ))
        })
        .collect()
}

fn format_extension_detail(data: &Value, i18n: &I18n) -> Vec<String> {
    let keys = [
        ("name", "Name", "名称"),
        ("version", "Version", "版本"),
        ("source", "Source", "来源"),
        ("path", "Path", "路径"),
        ("health", "Health", "健康状态"),
        ("activation", "Activation", "生效边界"),
    ];
    let mut lines = keys
        .into_iter()
        .filter_map(|(key, en, zh)| {
            data.get(key).map(|value| {
                let label = tr(i18n, en, zh);
                format!("  {label}: {}", display(value))
            })
        })
        .collect::<Vec<_>>();
    if let Some(active) = data.get("is_active") {
        lines.push(format!(
            "  {}: {}",
            tr(i18n, "Active", "已启用"),
            display(active)
        ));
    }
    if let Some(servers) = data.get("mcp_servers").and_then(Value::as_array) {
        lines.push(format!(
            "  {}: {}",
            tr(i18n, "MCP servers", "MCP 服务"),
            servers.len()
        ));
    }
    if let Some(agents) = data.get("agents").and_then(Value::as_array) {
        let executable = agents
            .iter()
            .filter(|agent| {
                agent
                    .get("executable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count();
        lines.push(format!(
            "  {}: {} declared, {} executable",
            tr(i18n, "Agents", "Agents"),
            agents.len(),
            executable
        ));
    }
    lines
}

fn format_preflight(data: &Value, i18n: &I18n) -> Vec<String> {
    let id = field(data, "operation_id");
    let mut lines = vec![
        format!(
            "  {}: {}",
            tr(i18n, "Extension", "扩展"),
            field(data, "name")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Action", "操作"),
            field(data, "action")
        ),
        format!("  {}: {id}", tr(i18n, "Operation", "操作编号")),
        format!(
            "  {}: {}",
            tr(i18n, "Version", "版本"),
            field(data, "version")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Source", "来源"),
            field(data, "source_identity")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Revision", "修订版本"),
            field(data, "resolved_revision")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Content digest", "内容摘要"),
            field(data, "content_digest")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Capability fingerprint", "能力指纹"),
            field(data, "capability_fingerprint")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Capabilities", "能力"),
            array_field(data, "capabilities")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Added", "新增能力"),
            array_field(data, "capabilities_added")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Removed", "移除能力"),
            array_field(data, "capabilities_removed")
        ),
        format!(
            "  {}: {} / {} / {}",
            tr(i18n, "Expected state", "预期状态"),
            field(data, "expected_desired_state"),
            field(data, "expected_effective_state"),
            field(data, "expected_activation")
        ),
    ];
    if let Some(summary) = data.get("risk_summary") {
        lines.push(format!("  {}:", tr(i18n, "Risk categories", "风险分类")));
        for (key, en, zh) in [
            ("execution", "execution", "执行"),
            ("instruction", "instruction", "指令"),
            ("authorization", "authorization", "授权"),
            ("credential", "credential", "凭证"),
            ("filesystem", "filesystem", "文件系统"),
        ] {
            lines.push(format!(
                "    {}: {}",
                tr(i18n, en, zh),
                array_field(summary, key)
            ));
        }
    }
    lines.push(tr(
        i18n,
        &format!("Review and run /extensions consent {id}, or /extensions cancel {id}."),
        &format!(
            "确认能力范围后运行 /extensions consent {id}；放弃则运行 /extensions cancel {id}。"
        ),
    ));
    lines
}

fn format_mutation(data: &Value, i18n: &I18n) -> Vec<String> {
    vec![tr(
        i18n,
        &format!(
            "Extension operation {} completed; activation is {}.",
            field(data, "action"),
            field(data, "activation")
        ),
        &format!(
            "扩展操作 {} 已完成；生效边界为 {}。",
            field(data, "action"),
            field(data, "activation")
        ),
    )]
}

fn format_update_all(data: &Value, i18n: &I18n) -> Vec<String> {
    let mut lines = vec![
        format!(
            "operation={} status={}",
            field(data, "operation_id"),
            field(data, "status")
        ),
        tr(
            i18n,
            &format!(
                "Update summary: {}",
                display(data.get("summary").unwrap_or(&Value::Null))
            ),
            &format!(
                "批量更新汇总：{}",
                display(data.get("summary").unwrap_or(&Value::Null))
            ),
        ),
    ];
    if let Some(items) = data.get("items").and_then(Value::as_array) {
        for item in items {
            let name = field(item, "name");
            let outcome = field(item, "outcome");
            lines.push(format!("  {name}: {outcome}"));
            if outcome == "pending_consent" {
                let id = item
                    .get("preflight")
                    .map(|value| field(value, "operation_id"))
                    .unwrap_or_else(|| "?".to_string());
                lines.push(format!("    /extensions consent {id}"));
            }
        }
    }
    lines
}

fn format_settings_list(data: &Value, i18n: &I18n) -> Vec<String> {
    let Some(settings) = data.as_array() else {
        return vec![tr(
            i18n,
            "No extension settings declared.",
            "扩展未声明 settings。",
        )];
    };
    if settings.is_empty() {
        return vec![tr(
            i18n,
            "No extension settings declared.",
            "扩展未声明 settings。",
        )];
    }
    settings
        .iter()
        .map(|setting| {
            format!(
                "  {} = {} ({})",
                field(setting, "key"),
                safe_setting_display(setting),
                field(setting, "setting_type")
            )
        })
        .collect()
}

fn format_setting(data: &Value, i18n: &I18n) -> Vec<String> {
    vec![
        format!("  {}: {}", tr(i18n, "Key", "键"), field(data, "key")),
        format!(
            "  {}: {}",
            tr(i18n, "Value", "值"),
            safe_setting_display(data)
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Scope", "作用域"),
            field(data, "scope")
        ),
        format!(
            "  {}: {}",
            tr(i18n, "Configured", "已配置"),
            field(data, "configured")
        ),
    ]
}

fn safe_setting_display(setting: &Value) -> String {
    if setting
        .get("sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if setting
            .get("configured")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return "[redacted]".to_string();
        }
        return "[not configured]".to_string();
    }
    setting
        .get("display")
        .and_then(Value::as_str)
        .unwrap_or("[not configured]")
        .to_string()
}

fn format_doctor(data: &Value, i18n: &I18n) -> Vec<String> {
    let diagnostics = data
        .get("diagnostics")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let agent_diagnostics = data
        .get("agent_diagnostics")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let runtime_diagnostics = data
        .get("runtime_diagnostics")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let total = diagnostics + agent_diagnostics + runtime_diagnostics;
    vec![tr(
        i18n,
        &format!("Extension doctor completed with {total} diagnostic(s)."),
        &format!("扩展诊断完成，共 {total} 条诊断。"),
    )]
}

fn help(i18n: &I18n) -> Vec<String> {
    let commands = [
        "list | info <name> | doctor [name]",
        "new <path> [--template minimal|skill|hook|mcp|context|agent]",
        "install <path|https-url> [--ref <ref>] | link <path>",
        "update <name> | update --all | uninstall <name>",
        "enable <name> | disable <name> | select-source <name> <user|system>",
        "settings list|get|set|unset <extension> ... [--scope user|workspace]",
        "operation <id> | consent <id> | cancel <id> | reload",
    ];
    let mut lines = vec![tr(i18n, "Available commands:", "可用命令：")];
    lines.extend(commands.into_iter().map(|command| format!("  {command}")));
    lines
}

fn string_field<'a>(data: &'a Value, key: &str) -> Result<&'a str, String> {
    data.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("registry response missing {key}"))
}

fn field(data: &Value, key: &str) -> String {
    data.get(key)
        .map(display)
        .unwrap_or_else(|| "?".to_string())
}

fn array_field(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().map(display).collect::<Vec<_>>().join(", "))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "none".to_string())
}

fn display(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn localized_error(i18n: &I18n, error: &str) -> String {
    tr(i18n, &format!("Error: {error}"), &format!("错误：{error}"))
}

fn tr(i18n: &I18n, en: &str, zh: &str) -> String {
    match i18n.language() {
        Language::EnUs => en.to_string(),
        Language::ZhCn => zh.to_string(),
    }
}
