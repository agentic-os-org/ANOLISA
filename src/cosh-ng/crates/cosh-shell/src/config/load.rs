use std::path::{Path, PathBuf};

use super::language::apply_language_value;
use super::parse::{parse_bool_value, parse_simple_config, parse_toml_config};
use super::trust::{load_project_trust_store, project_trust_store_path};
use super::CoshConfig;
use crate::types::CoshApprovalMode;

pub fn load_config() -> CoshConfig {
    let mut config = CoshConfig::default();

    if let Some(path) = config_read_file_path() {
        load_config_file_into(&path, &mut config);
    }
    if let Some(path) = project_trust_store_path() {
        if let Err(error) = load_project_trust_store(&mut config, &path) {
            tracing::error!("load project trust store failed: {error}");
        }
    }

    apply_env_overrides(&mut config);
    config
}

pub(super) fn load_config_file_into(path: &Path, config: &mut CoshConfig) {
    if let Ok(content) = std::fs::read_to_string(path) {
        parse_simple_config(&content, config);
        parse_toml_config(&content, config);
    }
}

pub(super) fn config_file_path() -> Option<PathBuf> {
    dirs_next_or_home().map(|d| d.join(".copilot-shell/config.toml"))
}

pub(super) fn config_read_file_path() -> Option<PathBuf> {
    config_file_path()
}

#[cfg(test)]
pub(super) fn config_read_file_path_for_home(home: &Path) -> PathBuf {
    home.join(".copilot-shell/config.toml")
}

pub(super) fn copilot_shell_cosh_dir() -> Option<PathBuf> {
    dirs_next_or_home().map(|d| d.join(".copilot-shell/cosh"))
}

pub(super) fn dirs_next_or_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

fn apply_env_overrides(config: &mut CoshConfig) {
    if let Ok(value) = std::env::var("COSH_SHELL_INTEGRATION") {
        config.shell_integration = value;
    }
    if let Ok(v) = std::env::var("COSH_SHELL_STATUS_SYMBOLS") {
        config.status_symbols = parse_bool_value(&v);
    }
    if let Ok(v) = std::env::var("COSH_SHELL_LOGIN_IDENTITY") {
        config.login_identity = parse_bool_value(&v);
    }
    if let Ok(v) = std::env::var("COSH_SHELL_ANALYSIS_MODE") {
        config.analysis_mode = v;
    }
    if let Ok(v) = std::env::var("COSH_SHELL_APPROVAL_MODE") {
        config.approval_mode = CoshApprovalMode::from_config(&v);
    }
    if let Ok(v) = std::env::var("COSH_SHELL_INPUT_WAIT_TIMEOUT_SECS") {
        if let Ok(secs) = v.trim().parse::<u64>() {
            config.input_wait_timeout_secs = secs;
        }
    }
    if let Ok(v) = std::env::var("COSH_SHELL_DEFAULT_SHELL") {
        config.shell_default = v;
    }
    if let Ok(v) = std::env::var("COSH_SHELL_ADAPTER") {
        config.adapter_default = v;
    } else if let Ok(v) = std::env::var("COSH_SHELL_ADAPTER_DEFAULT") {
        config.adapter_default = v;
    }
    if let Ok(v) = std::env::var("COSH_SHELL_AI") {
        config.ai_enabled = v != "off";
    }
    if let Ok(v) = std::env::var("COSH_SHELL_DEBUG") {
        config.debug = parse_bool_value(&v);
    }
    if let Ok(v) = std::env::var("COSH_LOG") {
        config.log_level = v;
    }
    // debug: true → map to "debug" level if log_level was not explicitly set
    if config.debug && config.log_level == "warn" {
        config.log_level = "debug".to_string();
    }
    if let Ok(v) = std::env::var("COSH_SHELL_LANG") {
        apply_language_value(config, &v);
    }
    if let Ok(v) = std::env::var("COSH_RECOMMENDATIONS_BASH_HISTORY") {
        config.recommendations.bash_history = parse_bool_value(&v);
    }
}

pub(crate) fn parse_recommendations_environment_override(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
