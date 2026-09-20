use std::path::PathBuf;

use crate::tools::readonly_rules::RuntimeReadonlyConfig;
use crate::types::CoshApprovalMode;

#[derive(Debug, Clone)]
pub struct CoshConfig {
    pub shell_default: String,
    pub shell_integration: String,
    /// Shows the `◇ `/`◌ ` input-ownership status line above each Enhanced
    /// prompt. Off by default so the child shell keeps its native prompt
    /// appearance; `COSH_SHELL_STATUS_SYMBOLS` overrides per session.
    pub status_symbols: bool,
    pub analysis_mode: String,
    pub approval_mode: CoshApprovalMode,
    pub adapter_default: String,
    pub language: String,
    pub startup_banner: bool,
    pub startup_hooks: bool,
    pub debug: bool,
    pub log_level: String,
    pub ai_enabled: bool,
    /// #2161: seconds an agent handoff may sit in a kernel-evidenced
    /// input-wait before the foreground group is interrupted; 0 disables.
    pub input_wait_timeout_secs: u64,
    pub health: HealthConfig,
    pub recommendations: RecommendationsConfig,
    pub trusted_commands: Vec<String>,
    pub trusted_project_roots: Vec<PathBuf>,
    pub(super) readonly: RuntimeReadonlyConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecommendationsConfig {
    pub enabled: bool,
    pub bash_history: bool,
}

impl Default for RecommendationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bash_history: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthConfig {
    pub enabled: bool,
    pub role: Option<String>,
    pub memory_sensitive: bool,
    pub critical_mounts: Vec<String>,
    pub verbose: bool,
    pub services: Vec<HealthServiceConfig>,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            role: None,
            memory_sensitive: false,
            critical_mounts: vec!["/".to_string()],
            verbose: false,
            services: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthServiceConfig {
    pub name: String,
    pub expected: HealthServiceExpectedState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthServiceExpectedState {
    Active,
    Inactive,
}

impl HealthServiceExpectedState {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" => Some(Self::Active),
            "inactive" => Some(Self::Inactive),
            _ => None,
        }
    }
}

impl Default for CoshConfig {
    fn default() -> Self {
        Self {
            shell_default: "auto".into(),
            shell_integration: "enhanced".into(),
            status_symbols: false,
            analysis_mode: "smart".into(),
            approval_mode: CoshApprovalMode::Auto,
            adapter_default: "cosh-core".into(),
            language: "auto".into(),
            startup_banner: true,
            startup_hooks: false,
            debug: false,
            log_level: "warn".into(),
            ai_enabled: true,
            input_wait_timeout_secs: 120,
            health: HealthConfig::default(),
            recommendations: RecommendationsConfig::default(),
            trusted_commands: Vec::new(),
            trusted_project_roots: Vec::new(),
            readonly: RuntimeReadonlyConfig::default(),
        }
    }
}

impl CoshConfig {
    pub(crate) fn readonly_config(&self) -> &RuntimeReadonlyConfig {
        &self.readonly
    }
}
