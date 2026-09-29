use std::path::PathBuf;

use super::language::apply_language_value;
use super::readonly::{parse_disabled_rules, parse_runtime_spec, string_array};
use super::{CoshConfig, HealthServiceConfig, HealthServiceExpectedState};
use crate::types::CoshApprovalMode;

pub(super) fn parse_simple_config(content: &str, config: &mut CoshConfig) {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.contains('=') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "shell.default" => config.shell_default = value.into(),
                "shell.integration" => config.shell_integration = value.into(),
                "shell.status_symbols" => config.status_symbols = parse_bool_value(value),
                "shell.login_identity" => config.login_identity = parse_bool_value(value),
                "shell.analysis_mode" => config.analysis_mode = value.into(),
                "shell.approval_mode" => {
                    config.approval_mode = CoshApprovalMode::from_config(value)
                }
                "shell.adapter_default" => config.adapter_default = value.into(),
                // #2161: invalid values fall back to the built-in default
                // (treat-as-unset), never to 0/disabled.
                "shell.input_wait_timeout_secs" => {
                    if let Ok(secs) = value.trim().parse::<u64>() {
                        config.input_wait_timeout_secs = secs;
                    }
                }
                "shell.trusted_command" if !value.is_empty() => {
                    config.trusted_commands.push(value.into());
                }
                "shell.trusted_project_root" if !value.is_empty() => {
                    config.trusted_project_roots.push(PathBuf::from(value));
                }
                "ui.language" => apply_language_value(config, value),
                "ui.startup_banner" => config.startup_banner = parse_bool_value(value),
                "ui.startup_hooks" => config.startup_hooks = parse_bool_value(value),
                "ui.debug" => config.debug = parse_bool_value(value),
                "ui.log_level" => config.log_level = value.to_string(),
                "health.enabled" => config.health.enabled = parse_bool_value(value),
                "health.role" => {
                    config.health.role = non_empty_string(value);
                }
                "health.memory_sensitive" => {
                    config.health.memory_sensitive = parse_bool_value(value);
                }
                "health.verbose" => config.health.verbose = parse_bool_value(value),
                _ => {}
            }
        }
    }
}

pub(super) fn parse_toml_config(content: &str, config: &mut CoshConfig) {
    let value = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(err) => {
            if content.contains("shell.readonly") || content.contains("readonly_disabled") {
                config
                    .readonly
                    .errors
                    .push(format!("invalid readonly config TOML: {err}"));
            }
            return;
        }
    };
    if let Some(ui) = value.get("ui").and_then(toml::Value::as_table) {
        if let Some(language) = ui.get("language").and_then(toml::Value::as_str) {
            apply_language_value(config, language);
        }
        if let Some(startup_banner) = ui.get("startup_banner").and_then(toml::Value::as_bool) {
            config.startup_banner = startup_banner;
        }
        if let Some(startup_hooks) = ui.get("startup_hooks").and_then(toml::Value::as_bool) {
            config.startup_hooks = startup_hooks;
        }
        if let Some(debug) = ui.get("debug").and_then(toml::Value::as_bool) {
            config.debug = debug;
        }
        if let Some(log_level) = ui.get("log_level").and_then(toml::Value::as_str) {
            config.log_level = log_level.to_string();
        }
    }
    parse_shell_toml_config(&value, config);
    parse_health_toml_config(&value, config);
}

fn parse_recommendations_toml_config(
    shell: &toml::map::Map<String, toml::Value>,
    config: &mut CoshConfig,
) {
    let Some(recommendations) = shell.get("recommendations").and_then(toml::Value::as_table) else {
        return;
    };
    if let Some(enabled) = recommendations
        .get("enabled")
        .and_then(toml::Value::as_bool)
    {
        config.recommendations.enabled = enabled;
    }
    if let Some(bash_history) = recommendations
        .get("bash_history")
        .and_then(toml::Value::as_bool)
    {
        config.recommendations.bash_history = bash_history;
    }
}

fn parse_shell_toml_config(value: &toml::Value, config: &mut CoshConfig) {
    let Some(shell) = value.get("shell").and_then(toml::Value::as_table) else {
        return;
    };

    if let Some(default) = shell.get("default").and_then(toml::Value::as_str) {
        config.shell_default = default.to_string();
    }
    if let Some(integration) = shell.get("integration").and_then(toml::Value::as_str) {
        config.shell_integration = integration.to_string();
    }
    if let Some(status_symbols) = shell.get("status_symbols").and_then(toml::Value::as_bool) {
        config.status_symbols = status_symbols;
    }
    // #R2: `[shell]` table form of the login-identity gate. Native TOML bool,
    // so the documented rollback `login_identity = false` also works here (not
    // only in the dotted-key form parsed by parse_simple_config).
    if let Some(login_identity) = shell.get("login_identity").and_then(toml::Value::as_bool) {
        config.login_identity = login_identity;
    }
    if let Some(analysis_mode) = shell.get("analysis_mode").and_then(toml::Value::as_str) {
        config.analysis_mode = analysis_mode.to_string();
    }
    if let Some(approval_mode) = shell.get("approval_mode").and_then(toml::Value::as_str) {
        config.approval_mode = CoshApprovalMode::from_config(approval_mode);
    }
    if let Some(adapter_default) = shell.get("adapter_default").and_then(toml::Value::as_str) {
        config.adapter_default = adapter_default.to_string();
    }
    // #2161: `[shell]` table form of `input_wait_timeout_secs`. Invalid or
    // non-integer values fall back to the built-in default (treat-as-unset),
    // never to 0/disabled — same contract as the dotted-key form above.
    if let Some(secs) = shell
        .get("input_wait_timeout_secs")
        .and_then(toml::Value::as_integer)
    {
        if let Ok(secs) = u64::try_from(secs) {
            config.input_wait_timeout_secs = secs;
        }
    }
    parse_recommendations_toml_config(shell, config);
    if let Some(commands) = shell.get("trusted_commands") {
        match string_array(commands, "shell.trusted_commands") {
            Ok(commands) => config
                .trusted_commands
                .extend(commands.into_iter().filter(|command| !command.is_empty())),
            Err(err) => config.readonly.errors.push(err),
        }
    }
    parse_trusted_project_roots(config, shell, "shell.trusted_project_roots");
    if let Some(disabled) = shell.get("readonly_disabled") {
        match parse_disabled_rules(disabled, "shell.readonly_disabled") {
            Ok(keys) => config.readonly.disabled.extend(keys),
            Err(err) => config.readonly.errors.push(err),
        }
    }

    parse_readonly_table(
        config,
        shell.get("readonly").and_then(toml::Value::as_table),
        "shell.readonly",
    );
}

fn parse_trusted_project_roots(
    config: &mut CoshConfig,
    table: &toml::map::Map<String, toml::Value>,
    label: &str,
) {
    let Some(roots) = table.get("trusted_project_roots") else {
        return;
    };
    match string_array(roots, label) {
        Ok(roots) => config
            .trusted_project_roots
            .extend(roots.into_iter().map(PathBuf::from)),
        Err(err) => config.readonly.errors.push(err),
    }
}

fn parse_readonly_table(
    config: &mut CoshConfig,
    readonly: Option<&toml::map::Map<String, toml::Value>>,
    label: &str,
) {
    let Some(readonly) = readonly else {
        return;
    };
    for (command, spec_value) in readonly {
        match parse_runtime_spec(command, spec_value, label) {
            Ok(Some(spec)) => config.readonly.overrides.push(spec),
            Ok(None) => {}
            Err(err) => config.readonly.errors.push(err),
        }
    }
}

fn parse_health_toml_config(value: &toml::Value, config: &mut CoshConfig) {
    let Some(health) = value.get("health").and_then(toml::Value::as_table) else {
        return;
    };

    if let Some(enabled) = health.get("enabled").and_then(toml::Value::as_bool) {
        config.health.enabled = enabled;
    }
    if let Some(role) = health.get("role").and_then(toml::Value::as_str) {
        config.health.role = non_empty_string(role);
    }
    if let Some(memory_sensitive) = health
        .get("memory_sensitive")
        .and_then(toml::Value::as_bool)
    {
        config.health.memory_sensitive = memory_sensitive;
    }
    if let Some(verbose) = health.get("verbose").and_then(toml::Value::as_bool) {
        config.health.verbose = verbose;
    }
    if let Some(critical_mounts) = health.get("critical_mounts") {
        if let Ok(mounts) = string_array(critical_mounts, "health.critical_mounts") {
            let mounts = mounts
                .into_iter()
                .filter(|mount| !mount.trim().is_empty())
                .collect::<Vec<_>>();
            if !mounts.is_empty() {
                config.health.critical_mounts = mounts;
            }
        }
    }
    if let Some(services) = health.get("services").and_then(toml::Value::as_array) {
        config.health.services = services
            .iter()
            .filter_map(parse_health_service)
            .collect::<Vec<_>>();
    }
}

fn parse_health_service(value: &toml::Value) -> Option<HealthServiceConfig> {
    let table = value.as_table()?;
    let name = table.get("name").and_then(toml::Value::as_str)?.trim();
    if name.is_empty() {
        return None;
    }
    let expected = table
        .get("expected")
        .and_then(toml::Value::as_str)
        .and_then(HealthServiceExpectedState::parse)
        .unwrap_or(HealthServiceExpectedState::Active);
    Some(HealthServiceConfig {
        name: name.to_string(),
        expected,
    })
}

fn non_empty_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub(super) fn parse_bool_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}
