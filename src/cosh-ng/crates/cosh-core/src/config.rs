use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canonical approval policy used at every Core input boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Apply the conservative policy previously exposed as strict or balanced.
    #[default]
    Recommend,
    /// Run eligible local tools and ask before guarded operations.
    Auto,
    /// Run registered tools without ordinary approval prompts.
    Trust,
}

impl ApprovalMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "recommend" | "balanced" | "strict" | "suggest" => Some(Self::Recommend),
            "auto" => Some(Self::Auto),
            "trust" => Some(Self::Trust),
            _ => None,
        }
    }

    pub(crate) fn from_config(value: &str) -> Self {
        match Self::parse(value) {
            Some(mode) => mode,
            None => {
                eprintln!("[cosh-core] Warning: invalid approval mode {value:?}; using recommend");
                Self::Recommend
            }
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Recommend => "recommend",
            Self::Auto => "auto",
            Self::Trust => "trust",
        }
    }
}

impl fmt::Display for ApprovalMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

impl FromStr for ApprovalMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or_else(|| {
            format!("invalid approval mode {value:?}; expected recommend, auto, or trust")
        })
    }
}

impl<'de> Deserialize<'de> for ApprovalMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CoreConfig {
    #[serde(default)]
    pub ai: AiConfig,
    // User-layer AI state is the only source for writing ~/.copilot-shell/config.toml.
    #[serde(skip)]
    pub(crate) user_ai: AiConfig,
    // System-layer AI state is used to expose provider ownership without persisting it.
    #[serde(skip)]
    pub(crate) system_ai: AiConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Trusted MCP client connections loaded from system or user configuration.
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AiConfig {
    pub active_provider: Option<String>,
    pub active_model: Option<String>,
    pub output_language: Option<String>,
    pub thinking: Option<String>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProviderConfig {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub provider_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// SysOM API host, honoured only by the `aliyun` provider type.
    ///
    /// Separate from `base_url` because that one is OpenAI-compatible and
    /// defaults to a DashScope address, which is not a SysOM endpoint. Unset,
    /// the client prefers the VPC proxy when reachable and the public host
    /// otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sysom_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_key_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit_cache: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_session_token_limit")]
    pub session_token_limit: u64,
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls_per_turn: u32,
    /// Tools that bypass approval for this agent configuration.
    #[serde(default)]
    pub allowed_tools: HashSet<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            approval_mode: ApprovalMode::default(),
            max_turns: default_max_turns(),
            session_token_limit: default_session_token_limit(),
            max_tool_calls_per_turn: default_max_tool_calls(),
            allowed_tools: HashSet::new(),
        }
    }
}

fn default_max_turns() -> u32 {
    50
}
fn default_session_token_limit() -> u64 {
    128_000
}
fn default_max_tool_calls() -> u32 {
    10
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Last explicit value applied by a trusted configuration layer.
    ///
    /// This distinguishes the default `false` from a system/user-requested
    /// disable so installed extensions can auto-enable hooks without allowing
    /// untrusted project configuration to override the kill switch.
    #[serde(skip)]
    pub(crate) enabled_override: Option<bool>,
    #[serde(default, rename = "PreToolUse")]
    pub pre_tool_use: Vec<HookDefinition>,
    #[serde(default, rename = "PostToolUse")]
    pub post_tool_use: Vec<HookDefinition>,
    #[serde(default, rename = "PostToolUseFailure")]
    pub post_tool_use_failure: Vec<HookDefinition>,
    #[serde(default, rename = "UserPromptSubmit")]
    pub user_prompt_submit: Vec<HookDefinition>,
    #[serde(default, rename = "SessionStart")]
    pub session_start: Vec<HookDefinition>,
    #[serde(default, rename = "Stop")]
    pub stop: Vec<HookDefinition>,
    #[serde(default, rename = "BeforeModel")]
    pub before_model: Vec<HookDefinition>,
    #[serde(default, rename = "AfterModel")]
    pub after_model: Vec<HookDefinition>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookDefinition {
    pub command: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub sequential: Option<bool>,
    /// Allow this hook to fail open. The default is fail closed because a
    /// PreToolUse hook failure must never silently authorize a tool call.
    #[serde(default)]
    pub fail_open: bool,
    /// Environment variables injected into this hook's child process only.
    /// The child inherits the parent environment first, so an entry here
    /// overrides an inherited value of the same name. The host never calls
    /// `std::env::set_var`, so the cosh-ng process itself is unaffected.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Validates an environment variable name against POSIX rules
/// (`[A-Za-z_][A-Za-z0-9_]*`). Only names are checked — values are opaque and
/// must never be logged.
pub fn is_valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SkillsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub custom_paths: Vec<String>,
}

/// Configuration for locally managed MCP client connections.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpConfig {
    /// Server definitions keyed by a stable, user-visible server name.
    #[serde(default)]
    pub servers: HashMap<String, McpServerConfig>,
}

/// A trusted MCP server that cosh-core may start locally or contact over HTTP.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// Executable for a locally managed stdio server, launched without a shell.
    #[serde(default)]
    pub command: String,
    /// Streamable HTTP endpoint. Mutually exclusive with `command`.
    #[serde(default)]
    pub url: Option<String>,
    /// Arguments passed to the configured executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Explicit environment variables available to the child process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Optional static bearer token for an HTTP server.
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// OAuth settings for a Streamable HTTP server. Tokens are stored separately.
    #[serde(default)]
    pub oauth: McpOAuthConfig,
    /// Startup and request timeout in milliseconds.
    #[serde(default = "default_mcp_timeout_ms")]
    pub timeout_ms: u64,
    /// Server startup and tool discovery timeout in milliseconds.
    #[serde(default = "default_mcp_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
    /// `None` exposes every server tool; an empty list exposes none.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
}

/// Non-secret OAuth settings for a Streamable HTTP MCP server.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpOAuthConfig {
    /// Pre-registered public client identifier. When absent, dynamic registration is used.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Requested OAuth scopes when the server does not advertise them.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// OAuth resource indicator. Defaults to the MCP endpoint.
    #[serde(default)]
    pub resource: Option<String>,
    /// Authorization-server metadata URL, bypassing protected-resource discovery.
    #[serde(default)]
    pub auth_server_metadata_url: Option<String>,
    /// Local callback port. An ephemeral port is used when omitted.
    #[serde(default)]
    pub callback_port: Option<u16>,
}

fn default_mcp_timeout_ms() -> u64 {
    10_000
}

fn default_mcp_startup_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_true")]
    pub auto_persist: bool,
    #[serde(default = "default_persist_dir")]
    pub persist_dir: String,
    #[serde(default)]
    pub compaction: CompactionConfig,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            auto_persist: true,
            persist_dir: default_persist_dir(),
            compaction: CompactionConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
/// Model-aware session context compaction policy (`[session.compaction]`).
pub struct CompactionConfig {
    /// Master switch for manual and automatic compaction.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether idle-boundary automatic compaction may trigger.
    #[serde(default = "default_true")]
    pub auto: bool,
    /// Optional absolute trigger bound; always clamped to the model budget.
    #[serde(default)]
    pub auto_compact_token_limit: Option<u64>,
    /// Fraction of the usable history budget that triggers normal compaction.
    #[serde(default = "default_trigger_ratio")]
    pub trigger_ratio: f64,
    /// Fraction of the usable history budget that arms emergency protection.
    #[serde(default = "default_emergency_ratio")]
    pub emergency_ratio: f64,
    /// Best-effort post-compaction fraction of the usable history budget.
    #[serde(default = "default_target_ratio")]
    pub target_ratio: f64,
    /// Minimum recent complete Agent runs kept verbatim by normal automatic
    /// compaction.
    ///
    /// Explicit manual compaction may summarize the latest complete run. So may
    /// emergency protection as a last resort: rather than failing the request,
    /// its fallback ladder degrades this value to one and then to
    /// manual-compaction semantics, which reserves no complete run
    /// unconditionally. The in-flight (incomplete) run is never summarized by
    /// any path.
    #[serde(default = "default_preserve_recent_runs")]
    pub preserve_recent_runs: usize,
    /// Explicit user override for the model context window, in tokens.
    #[serde(default)]
    pub model_context_window: Option<u64>,
    /// Explicit user override for the model's maximum output size, in tokens.
    ///
    /// Drives both sides of the output accounting from one value: the reserve
    /// `O` subtracted from the usable history budget, and the `max_tokens` cap
    /// sent on the real provider request. Lowering it therefore frees history
    /// budget *and* shortens the longest reply the model may produce.
    #[serde(default)]
    pub model_max_output_tokens: Option<u64>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto: true,
            auto_compact_token_limit: None,
            trigger_ratio: default_trigger_ratio(),
            emergency_ratio: default_emergency_ratio(),
            target_ratio: default_target_ratio(),
            preserve_recent_runs: default_preserve_recent_runs(),
            model_context_window: None,
            model_max_output_tokens: None,
        }
    }
}

impl CompactionConfig {
    /// Replaces unusable ratio overrides with the compiled-in defaults.
    ///
    /// TOML happily deserializes `nan`, `inf`, and `-inf` into `f64`, and
    /// `f64::clamp` panics when a bound is NaN — a project-level
    /// `.copilot-shell/config.toml` must never be able to crash every Agent
    /// turn. Non-finite or out-of-range fields fall back individually; a
    /// combination that cannot satisfy `target <= trigger <= emergency` falls
    /// back as a whole group so the documented 70/90/30 semantics hold.
    pub(crate) fn sanitize_ratios(&mut self) {
        fn sanitize_field(name: &str, value: &mut f64, default: f64) {
            if value.is_finite() && (0.0..=1.0).contains(value) {
                return;
            }
            eprintln!(
                "[cosh-core] Warning: [session.compaction] {name} = {value} is not a \
                 finite ratio in [0, 1]; using default {default}"
            );
            *value = default;
        }
        sanitize_field(
            "trigger_ratio",
            &mut self.trigger_ratio,
            DEFAULT_TRIGGER_RATIO,
        );
        sanitize_field(
            "emergency_ratio",
            &mut self.emergency_ratio,
            DEFAULT_EMERGENCY_RATIO,
        );
        sanitize_field("target_ratio", &mut self.target_ratio, DEFAULT_TARGET_RATIO);
        if !(self.target_ratio <= self.trigger_ratio && self.trigger_ratio <= self.emergency_ratio)
        {
            eprintln!(
                "[cosh-core] Warning: [session.compaction] ratios cannot satisfy \
                 target <= trigger <= emergency ({} / {} / {}); using default policy",
                self.target_ratio, self.trigger_ratio, self.emergency_ratio
            );
            self.trigger_ratio = DEFAULT_TRIGGER_RATIO;
            self.emergency_ratio = DEFAULT_EMERGENCY_RATIO;
            self.target_ratio = DEFAULT_TARGET_RATIO;
        }
    }
}

/// Default normal automatic trigger fraction of the usable history budget.
pub(crate) const DEFAULT_TRIGGER_RATIO: f64 = 0.70;
/// Default emergency protection fraction of the usable history budget.
pub(crate) const DEFAULT_EMERGENCY_RATIO: f64 = 0.90;
/// Default best-effort post-compaction fraction of the usable history budget.
pub(crate) const DEFAULT_TARGET_RATIO: f64 = 0.30;

fn default_trigger_ratio() -> f64 {
    DEFAULT_TRIGGER_RATIO
}
fn default_emergency_ratio() -> f64 {
    DEFAULT_EMERGENCY_RATIO
}
fn default_target_ratio() -> f64 {
    DEFAULT_TARGET_RATIO
}
fn default_preserve_recent_runs() -> usize {
    2
}

fn default_true() -> bool {
    true
}
pub(crate) const DEFAULT_SESSION_PERSIST_DIR: &str = "~/.copilot-shell/cosh-core/sessions";

fn default_persist_dir() -> String {
    DEFAULT_SESSION_PERSIST_DIR.to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LoggingConfig {
    pub level: Option<String>,
}

impl LoggingConfig {
    pub fn effective_level(&self, verbose: bool) -> String {
        if let Ok(v) = std::env::var("COSH_LOG") {
            return v;
        }
        if verbose {
            return "debug".to_string();
        }
        self.level.clone().unwrap_or_else(|| "info".to_string())
    }
}

pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".copilot-shell")
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialCoreConfig {
    ai: Option<PartialAiConfig>,
    agent: Option<PartialAgentConfig>,
    hooks: Option<PartialHooksConfig>,
    skills: Option<PartialSkillsConfig>,
    mcp: Option<McpConfig>,
    session: Option<PartialSessionConfig>,
    logging: Option<PartialLoggingConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialAiConfig {
    active_provider: Option<String>,
    active_model: Option<String>,
    output_language: Option<String>,
    thinking: Option<String>,
    #[serde(default)]
    providers: HashMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialAgentConfig {
    approval_mode: Option<String>,
    max_turns: Option<u32>,
    session_token_limit: Option<u64>,
    max_tool_calls_per_turn: Option<u32>,
    allowed_tools: Option<HashSet<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialHooksConfig {
    enabled: Option<bool>,
    #[serde(rename = "PreToolUse")]
    pre_tool_use: Option<Vec<HookDefinition>>,
    #[serde(rename = "PostToolUse")]
    post_tool_use: Option<Vec<HookDefinition>>,
    #[serde(rename = "PostToolUseFailure")]
    post_tool_use_failure: Option<Vec<HookDefinition>>,
    #[serde(rename = "UserPromptSubmit")]
    user_prompt_submit: Option<Vec<HookDefinition>>,
    #[serde(rename = "SessionStart")]
    session_start: Option<Vec<HookDefinition>>,
    #[serde(rename = "Stop")]
    stop: Option<Vec<HookDefinition>>,
    #[serde(rename = "BeforeModel")]
    before_model: Option<Vec<HookDefinition>>,
    #[serde(rename = "AfterModel")]
    after_model: Option<Vec<HookDefinition>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialSkillsConfig {
    enabled: Option<bool>,
    custom_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialSessionConfig {
    auto_persist: Option<bool>,
    persist_dir: Option<String>,
    compaction: Option<PartialCompactionConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialCompactionConfig {
    enabled: Option<bool>,
    auto: Option<bool>,
    auto_compact_token_limit: Option<u64>,
    trigger_ratio: Option<f64>,
    emergency_ratio: Option<f64>,
    target_ratio: Option<f64>,
    preserve_recent_runs: Option<usize>,
    model_context_window: Option<u64>,
    model_max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialLoggingConfig {
    level: Option<String>,
}

fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            let replacement = std::env::var(var_name).unwrap_or_default();
            result = format!(
                "{}{}{}",
                &result[..start],
                replacement,
                &result[start + end + 1..]
            );
        } else {
            break;
        }
    }
    result
}

fn read_partial_config(path: &std::path::Path) -> Option<PartialCoreConfig> {
    if !path.exists() {
        return None;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!(
                "[cosh-core] Warning: failed to read {}: {}",
                path.display(),
                e
            );
            return None;
        }
    };

    match toml::from_str::<PartialCoreConfig>(&content) {
        Ok(config) => Some(config),
        Err(e) => {
            eprintln!(
                "[cosh-core] Warning: failed to parse {}: {}",
                path.display(),
                e
            );
            None
        }
    }
}

fn apply_user_layer(config: &mut CoreConfig, layer: &PartialCoreConfig) {
    if let Some(ref ai) = layer.ai {
        if let Some(ref value) = ai.active_provider {
            config.ai.active_provider = Some(value.clone());
        }
        apply_ai_preferences(&mut config.ai, ai);
        for (provider_id, provider) in &ai.providers {
            config
                .ai
                .providers
                .insert(provider_id.clone(), provider.clone());
        }
    }
    if let Some(ref mcp) = layer.mcp {
        config.mcp.servers.extend(mcp.servers.clone());
    }
    apply_common_layers(config, layer, true);
}

fn apply_project_layer(config: &mut CoreConfig, layer: &PartialCoreConfig, path: &std::path::Path) {
    if let Some(ref ai) = layer.ai {
        if ai.active_provider.is_some() {
            eprintln!(
                "[cosh-core] Warning: ignoring active_provider from project config {}",
                path.display()
            );
        }
        if !ai.providers.is_empty() {
            eprintln!(
                "[cosh-core] Warning: ignoring ai.providers from project config {}",
                path.display()
            );
        }
        apply_ai_preferences(&mut config.ai, ai);
    }
    if layer.mcp.is_some() {
        eprintln!(
            "[cosh-core] Warning: ignoring MCP servers from project config {}",
            path.display()
        );
    }
    apply_common_layers(config, layer, false);
}

fn apply_ai_preferences(ai: &mut AiConfig, layer: &PartialAiConfig) {
    if let Some(ref value) = layer.active_model {
        ai.active_model = Some(value.clone());
    }
    if let Some(ref value) = layer.output_language {
        ai.output_language = Some(value.clone());
    }
    if let Some(ref value) = layer.thinking {
        ai.thinking = Some(value.clone());
    }
}

fn apply_common_layers(
    config: &mut CoreConfig,
    layer: &PartialCoreConfig,
    hooks_can_control_extensions: bool,
) {
    if let Some(ref agent) = layer.agent {
        apply_agent_layer(&mut config.agent, agent);
    }
    if let Some(ref hooks) = layer.hooks {
        apply_hooks_layer(&mut config.hooks, hooks, hooks_can_control_extensions);
    }
    if let Some(ref skills) = layer.skills {
        apply_skills_layer(&mut config.skills, skills);
    }
    if let Some(ref session) = layer.session {
        apply_session_layer(&mut config.session, session);
    }
    if let Some(ref logging) = layer.logging {
        apply_logging_layer(&mut config.logging, logging);
    }
}

fn apply_agent_layer(config: &mut AgentConfig, layer: &PartialAgentConfig) {
    if let Some(ref value) = layer.approval_mode {
        config.approval_mode = ApprovalMode::from_config(value);
    }
    if let Some(value) = layer.max_turns {
        config.max_turns = value;
    }
    if let Some(value) = layer.session_token_limit {
        config.session_token_limit = value;
    }
    if let Some(value) = layer.max_tool_calls_per_turn {
        config.max_tool_calls_per_turn = value;
    }
    if let Some(ref value) = layer.allowed_tools {
        config.allowed_tools = value.clone();
    }
}

fn apply_hooks_layer(
    config: &mut HooksConfig,
    layer: &PartialHooksConfig,
    can_control_extensions: bool,
) {
    if let Some(value) = layer.enabled {
        config.enabled = value;
        if can_control_extensions {
            config.enabled_override = Some(value);
        }
    }
    if let Some(ref value) = layer.pre_tool_use {
        config.pre_tool_use = value.clone();
    }
    if let Some(ref value) = layer.post_tool_use {
        config.post_tool_use = value.clone();
    }
    if let Some(ref value) = layer.post_tool_use_failure {
        config.post_tool_use_failure = value.clone();
    }
    if let Some(ref value) = layer.user_prompt_submit {
        config.user_prompt_submit = value.clone();
    }
    if let Some(ref value) = layer.session_start {
        config.session_start = value.clone();
    }
    if let Some(ref value) = layer.stop {
        config.stop = value.clone();
    }
    if let Some(ref value) = layer.before_model {
        config.before_model = value.clone();
    }
    if let Some(ref value) = layer.after_model {
        config.after_model = value.clone();
    }
}

fn apply_skills_layer(config: &mut SkillsConfig, layer: &PartialSkillsConfig) {
    if let Some(value) = layer.enabled {
        config.enabled = value;
    }
    if let Some(ref value) = layer.custom_paths {
        config.custom_paths = value.clone();
    }
}

fn apply_session_layer(config: &mut SessionConfig, layer: &PartialSessionConfig) {
    if let Some(value) = layer.auto_persist {
        config.auto_persist = value;
    }
    if let Some(ref value) = layer.persist_dir {
        config.persist_dir = value.clone();
    }
    // The compaction table is merged field-by-field so a higher-priority
    // layer that sets only one key does not reset the others to defaults.
    if let Some(ref value) = layer.compaction {
        apply_compaction_layer(&mut config.compaction, value);
    }
}

fn apply_compaction_layer(config: &mut CompactionConfig, layer: &PartialCompactionConfig) {
    if let Some(value) = layer.enabled {
        config.enabled = value;
    }
    if let Some(value) = layer.auto {
        config.auto = value;
    }
    if let Some(value) = layer.auto_compact_token_limit {
        config.auto_compact_token_limit = Some(value);
    }
    if let Some(value) = layer.trigger_ratio {
        config.trigger_ratio = value;
    }
    if let Some(value) = layer.emergency_ratio {
        config.emergency_ratio = value;
    }
    if let Some(value) = layer.target_ratio {
        config.target_ratio = value;
    }
    if let Some(value) = layer.preserve_recent_runs {
        config.preserve_recent_runs = value;
    }
    if let Some(value) = layer.model_context_window {
        config.model_context_window = Some(value);
    }
    if let Some(value) = layer.model_max_output_tokens {
        config.model_max_output_tokens = Some(value);
    }
}

fn apply_logging_layer(config: &mut LoggingConfig, layer: &PartialLoggingConfig) {
    if let Some(ref value) = layer.level {
        config.level = Some(value.clone());
    }
}

impl CoreConfig {
    pub fn load() -> Self {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::load_for_workspace(&workspace)
    }

    /// Loads the project layer from the workspace that owns the provider turn.
    pub fn load_for_workspace(workspace: &Path) -> Self {
        crate::migrate::try_migrate();

        let project_path = workspace.join(".copilot-shell/config.toml");
        let user_path = config_dir().join("config.toml");
        let system_path = PathBuf::from("/etc/copilot-shell/config.toml");

        let mut config =
            Self::load_from_paths(Some(&system_path), Some(&user_path), Some(&project_path));
        config.apply_env_overrides();
        config
    }

    pub fn load_bare() -> Self {
        crate::migrate::try_migrate();

        let user_path = config_dir().join("config.toml");
        let system_path = PathBuf::from("/etc/copilot-shell/config.toml");
        let mut config = Self::load_from_paths(Some(&system_path), Some(&user_path), None);
        config.apply_env_overrides();
        config.apply_bare_isolation();
        config
    }

    /// Loads provider credentials for Gateway brokered mode without migration.
    ///
    /// The launch profile is already fixed before this method is called. It
    /// deliberately excludes the project layer and clears every local runtime
    /// contribution so startup cannot run hooks, skills, MCP, or registry
    /// migration before the private v3 handshake.
    pub(crate) fn load_gateway_brokered() -> Self {
        let user_path = config_dir().join("config.toml");
        let system_path = PathBuf::from("/etc/copilot-shell/config.toml");
        let mut config = Self::load_from_paths(Some(&system_path), Some(&user_path), None);
        config.apply_env_overrides();
        config.apply_bare_isolation();
        config.mcp = McpConfig::default();
        config.agent.allowed_tools.clear();
        config.agent.approval_mode = ApprovalMode::Recommend;
        config
    }

    fn apply_bare_isolation(&mut self) {
        self.hooks = HooksConfig {
            enabled_override: Some(false),
            ..Default::default()
        };
        self.skills = SkillsConfig::default();
        self.session.auto_persist = false;
    }

    fn load_from_paths(
        system_path: Option<&std::path::Path>,
        user_path: Option<&std::path::Path>,
        project_path: Option<&std::path::Path>,
    ) -> Self {
        let mut config = CoreConfig::default();

        if let Some(system_path) = system_path {
            if let Some(system) = read_partial_config(system_path) {
                apply_user_layer(&mut config, &system);
                let mut system_snapshot = CoreConfig::default();
                apply_user_layer(&mut system_snapshot, &system);
                config.system_ai = system_snapshot.ai;
            }
        }

        if let Some(user_path) = user_path {
            if let Some(user) = read_partial_config(user_path) {
                apply_user_layer(&mut config, &user);
                let mut user_snapshot = CoreConfig::default();
                apply_user_layer(&mut user_snapshot, &user);
                user_snapshot.user_ai = user_snapshot.ai.clone();
                config.user_ai = user_snapshot.ai.clone();
            }
        }

        if let Some(project_path) = project_path {
            if let Some(project) = read_partial_config(project_path) {
                apply_project_layer(&mut config, &project, project_path);
            }
        }

        // Any layer (including an untrusted project config) may have written
        // non-finite or contradictory compaction ratios; validate once after
        // all layers so every consumer sees a policy that can never panic.
        config.session.compaction.sanitize_ratios();
        config
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(val) = std::env::var("COSH_APPROVAL_MODE") {
            self.agent.approval_mode = ApprovalMode::from_config(&val);
        }
        if let Ok(val) = std::env::var("COSH_MODEL") {
            self.ai.active_model = Some(val);
        }
        if let Ok(val) = std::env::var("COSH_AI_PROVIDER") {
            self.ai.active_provider = Some(val);
        }
        if let Ok(val) = std::env::var("COSH_OUTPUT_LANGUAGE") {
            self.ai.output_language = Some(val);
        }
        if let Ok(val) = std::env::var("COSH_MAX_TURNS") {
            if let Ok(n) = val.parse::<u32>() {
                self.agent.max_turns = n;
            }
        }
    }

    pub fn resolve_provider(&self) -> ResolvedProvider {
        let provider_name = self
            .ai
            .active_provider
            .clone()
            .unwrap_or_else(|| "default".to_string());

        let provider_cfg = self.ai.providers.get(&provider_name);

        // SysOM's API host, kept deliberately separate from `base_url`.
        //
        // `base_url` is an OpenAI-compatible concept: unset it falls back to the
        // DashScope compatible-mode address below. Feeding that into the SysOM
        // ACS3 path sent signed traffic to DashScope, so the two no longer share
        // a field. Empty here means "not configured", which lets the endpoint
        // resolver fall through to probing the VPC proxy.
        let sysom_endpoint = provider_cfg
            .and_then(|p| p.sysom_endpoint.as_deref())
            .map(expand_env_vars)
            .unwrap_or_default();

        let base_url = provider_cfg
            .and_then(|p| p.base_url.as_deref())
            .map(expand_env_vars)
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string());

        let api_key = provider_cfg
            .and_then(|p| p.api_key.as_deref())
            .map(expand_env_vars)
            .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .unwrap_or_default();

        let model = self
            .ai
            .active_model
            .clone()
            .or_else(|| provider_cfg.and_then(|p| p.model.clone()))
            .unwrap_or_else(|| "qwen-max".to_string());

        let provider_type = provider_cfg
            .and_then(|p| p.provider_type.clone())
            .unwrap_or_else(|| "generic".to_string());

        let extra_params = provider_cfg.and_then(|p| p.extra_params.clone());

        let auth_source = provider_cfg.and_then(|p| p.auth_source.clone());

        let access_key_id = provider_cfg
            .and_then(|p| p.access_key_id.as_deref())
            .map(expand_env_vars)
            .or_else(|| std::env::var("ALIBABA_CLOUD_ACCESS_KEY_ID").ok())
            .unwrap_or_default();

        let access_key_secret = provider_cfg
            .and_then(|p| p.access_key_secret.as_deref())
            .map(expand_env_vars)
            .or_else(|| std::env::var("ALIBABA_CLOUD_ACCESS_KEY_SECRET").ok())
            .unwrap_or_default();

        let security_token = provider_cfg
            .and_then(|p| p.security_token.as_deref())
            .map(expand_env_vars)
            .or_else(|| std::env::var("ALIBABA_CLOUD_SECURITY_TOKEN").ok());

        let explicit_cache = provider_cfg.and_then(|p| p.explicit_cache).unwrap_or(false);

        ResolvedProvider {
            base_url,
            sysom_endpoint,
            api_key,
            model,
            provider_type,
            auth_source,
            extra_params,
            access_key_id,
            access_key_secret,
            security_token,
            explicit_cache,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    pub base_url: String,
    /// SysOM API host from this provider's own configuration, or empty.
    ///
    /// Never holds an OpenAI-compatible URL: it has no default and no env
    /// fallback, so an empty value means the endpoint resolver should decide
    /// between the VPC proxy and the public host on its own.
    pub sysom_endpoint: String,
    pub api_key: String,
    pub model: String,
    pub provider_type: String,
    pub auth_source: Option<String>,
    pub extra_params: Option<Value>,
    pub access_key_id: String,
    pub access_key_secret: String,
    pub security_token: Option<String>,
    pub explicit_cache: bool,
}

impl ResolvedProvider {
    pub fn auth_required(&self) -> bool {
        if self.provider_type == "mock" {
            return false;
        }
        if self.provider_type == "aliyun" {
            return self.auth_source.as_deref() != Some("ecs_ram_role")
                && (self.access_key_id.is_empty() || self.access_key_secret.is_empty());
        }
        self.api_key.is_empty()
    }
}

/// Persist the current provider config to `~/.copilot-shell/config.toml`.
/// Only writes the [ai] section to avoid overwriting other settings.
pub fn persist_config(config: &CoreConfig) -> Result<(), String> {
    let dir = config_dir();
    persist_config_to_dir(config, &dir)
}

/// Matches only the `[ai]` table header and its dotted children (`[ai.providers.x]`),
/// not lookalike sections such as `[aider]` or `[ai_drivers]`.
/// Trailing whitespace or an inline comment after the closing bracket is accepted.
fn is_ai_section_header(line: &str) -> bool {
    let t = line.trim();
    let Some(end) = t.find(']') else {
        return false;
    };
    let rest = t[end + 1..].trim_start();
    if !(rest.is_empty() || rest.starts_with('#')) {
        return false;
    }
    let header = &t[..end + 1];
    header == "[ai]" || header.starts_with("[ai.")
}

fn persist_config_to_dir(config: &CoreConfig, dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create config dir: {e}"))?;

    let config_path = dir.join("config.toml");

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();

    let mut preserved = String::new();
    let mut in_ai_section = false;
    for line in existing.lines() {
        if is_ai_section_header(line) {
            in_ai_section = true;
            continue;
        }
        if in_ai_section && line.trim().starts_with('[') && !is_ai_section_header(line) {
            in_ai_section = false;
        }
        if !in_ai_section {
            preserved.push_str(line);
            preserved.push('\n');
        }
    }

    preserved.push_str("[ai]\n");
    if let Some(ref active) = config.user_ai.active_provider {
        preserved.push_str(&format!(
            "active_provider = \"{}\"\n",
            escape_toml_value(active)
        ));
    }
    if let Some(ref model) = config.user_ai.active_model {
        preserved.push_str(&format!(
            "active_model = \"{}\"\n",
            escape_toml_value(model)
        ));
    }
    if let Some(ref lang) = config.user_ai.output_language {
        preserved.push_str(&format!(
            "output_language = \"{}\"\n",
            escape_toml_value(lang)
        ));
    }
    if let Some(ref thinking) = config.user_ai.thinking {
        preserved.push_str(&format!("thinking = \"{}\"\n", escape_toml_value(thinking)));
    }
    preserved.push('\n');

    for (name, provider) in &config.user_ai.providers {
        preserved.push_str(&format!("[ai.providers.{}]\n", name));
        if let Some(ref t) = provider.provider_type {
            preserved.push_str(&format!("type = \"{}\"\n", escape_toml_value(t)));
        }
        if let Some(ref source) = provider.auth_source {
            preserved.push_str(&format!(
                "auth_source = \"{}\"\n",
                escape_toml_value(source)
            ));
        }
        if let Some(ref url) = provider.base_url {
            preserved.push_str(&format!("base_url = \"{}\"\n", escape_toml_value(url)));
        }
        if let Some(ref host) = provider.sysom_endpoint {
            preserved.push_str(&format!(
                "sysom_endpoint = \"{}\"\n",
                escape_toml_value(host)
            ));
        }
        if let Some(ref key) = provider.api_key {
            preserved.push_str(&format!("api_key = \"{}\"\n", escape_toml_value(key)));
        }
        if let Some(ref m) = provider.model {
            preserved.push_str(&format!("model = \"{}\"\n", escape_toml_value(m)));
        }
        if let Some(ref ak) = provider.access_key_id {
            preserved.push_str(&format!("access_key_id = \"{}\"\n", escape_toml_value(ak)));
        }
        if let Some(ref sk) = provider.access_key_secret {
            preserved.push_str(&format!(
                "access_key_secret = \"{}\"\n",
                escape_toml_value(sk)
            ));
        }
        if let Some(ref st) = provider.security_token {
            preserved.push_str(&format!("security_token = \"{}\"\n", escape_toml_value(st)));
        }
        if let Some(true) = provider.explicit_cache {
            preserved.push_str("explicit_cache = true\n");
        }
        preserved.push('\n');
    }

    let pid = std::process::id();
    let tmp_path = dir.join(format!("config.toml.tmp.{pid}"));
    std::fs::write(&tmp_path, &preserved).map_err(|e| format!("Failed to write config: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&tmp_path, perms);
    }

    std::fs::rename(&tmp_path, &config_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("Failed to rename config: {e}")
    })?;

    Ok(())
}

fn escape_toml_value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = CoreConfig::default();
        assert_eq!(config.agent.approval_mode, ApprovalMode::Recommend);
        assert_eq!(config.agent.max_turns, 50);
        assert_eq!(config.agent.session_token_limit, 128_000);
        assert_eq!(config.agent.max_tool_calls_per_turn, 10);
        assert!(config.session.auto_persist);
        assert_eq!(config.hooks.enabled_override, None);
    }

    #[test]
    fn layered_hooks_enabled_tracks_explicit_disable() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        std::fs::write(&user, "[hooks]\nenabled = false\n").unwrap();

        let config = CoreConfig::load_from_paths(None, Some(&user), None);

        assert!(!config.hooks.enabled);
        assert_eq!(config.hooks.enabled_override, Some(false));
    }

    #[test]
    fn project_hooks_disable_cannot_control_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        std::fs::write(&project, "[hooks]\nenabled = false\n").unwrap();

        let config = CoreConfig::load_from_paths(None, None, Some(&project));

        assert!(!config.hooks.enabled);
        assert_eq!(config.hooks.enabled_override, None);
    }

    #[test]
    fn project_hooks_enable_cannot_override_trusted_disable() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        let project = dir.path().join("project.toml");
        std::fs::write(&user, "[hooks]\nenabled = false\n").unwrap();
        std::fs::write(&project, "[hooks]\nenabled = true\n").unwrap();

        let config = CoreConfig::load_from_paths(None, Some(&user), Some(&project));

        assert!(config.hooks.enabled);
        assert_eq!(config.hooks.enabled_override, Some(false));
    }

    #[test]
    fn approval_mode_normalizes_legacy_aliases() {
        for value in ["recommend", "balanced", "strict", "suggest"] {
            assert_eq!(ApprovalMode::parse(value), Some(ApprovalMode::Recommend));
        }
        assert_eq!(ApprovalMode::parse("auto"), Some(ApprovalMode::Auto));
        assert_eq!(ApprovalMode::parse("trust"), Some(ApprovalMode::Trust));
        assert_eq!(ApprovalMode::parse("invalid"), None);
        assert_eq!(
            ApprovalMode::from_config("invalid"),
            ApprovalMode::Recommend
        );
    }

    #[test]
    fn layered_invalid_approval_mode_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system.toml");
        let user = dir.path().join("user.toml");
        std::fs::write(&system, "[agent]\napproval_mode = \"trust\"\n").unwrap();
        std::fs::write(&user, "[agent]\napproval_mode = \"invalid\"\n").unwrap();

        let config = CoreConfig::load_from_paths(Some(&system), Some(&user), None);

        assert_eq!(config.agent.approval_mode, ApprovalMode::Recommend);
    }

    #[test]
    fn parse_toml_config() {
        let toml_str = r#"
[ai]
active_provider = "qwen"
active_model = "qwen3-235b-a22b"
output_language = "zh-CN"

[ai.providers.qwen]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "sk-test"
model = "qwen3-235b-a22b"

[agent]
approval_mode = "trust"
max_turns = 50
session_token_limit = 256000
max_tool_calls_per_turn = 20
"#;
        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.ai.active_provider.as_deref(), Some("qwen"));
        assert_eq!(config.ai.active_model.as_deref(), Some("qwen3-235b-a22b"));
        assert_eq!(config.ai.output_language.as_deref(), Some("zh-CN"));

        let qwen = config.ai.providers.get("qwen").unwrap();
        assert_eq!(qwen.provider_type.as_deref(), Some("openai_compat"));
        assert_eq!(qwen.base_url.as_deref(), Some("https://example.com/v1"));
        assert_eq!(qwen.api_key.as_deref(), Some("sk-test"));

        assert_eq!(config.agent.approval_mode, ApprovalMode::Trust);
        assert_eq!(config.agent.max_turns, 50);
    }

    #[test]
    fn non_finite_compaction_ratios_from_toml_fall_back_to_defaults() {
        // A project `.copilot-shell/config.toml` is untrusted input: TOML
        // `nan`/`inf`/`-inf` deserialize into f64 and previously panicked the
        // budget clamp on every Agent turn. The load layer must fall back.
        for bad in ["nan", "inf", "-inf", "7.5", "-0.5"] {
            for field in ["trigger_ratio", "emergency_ratio", "target_ratio"] {
                let tmp = tempfile::TempDir::new().unwrap();
                let project_path = tmp.path().join("config.toml");
                std::fs::write(
                    &project_path,
                    format!("[session.compaction]\n{field} = {bad}\n"),
                )
                .unwrap();
                let config = CoreConfig::load_from_paths(None, None, Some(&project_path));
                let compaction = &config.session.compaction;
                assert!(
                    compaction.trigger_ratio.is_finite()
                        && compaction.emergency_ratio.is_finite()
                        && compaction.target_ratio.is_finite(),
                    "{field}={bad} left a non-finite ratio"
                );
                assert!(
                    compaction.target_ratio <= compaction.trigger_ratio
                        && compaction.trigger_ratio <= compaction.emergency_ratio,
                    "{field}={bad} broke threshold ordering"
                );
            }
        }
    }

    #[test]
    fn legal_compaction_ratio_overrides_survive_sanitization() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_path = tmp.path().join("config.toml");
        std::fs::write(
            &project_path,
            "[session.compaction]\ntrigger_ratio = 0.5\nemergency_ratio = 0.8\ntarget_ratio = 0.2\n",
        )
        .unwrap();
        let config = CoreConfig::load_from_paths(None, None, Some(&project_path));
        assert_eq!(config.session.compaction.trigger_ratio, 0.5);
        assert_eq!(config.session.compaction.emergency_ratio, 0.8);
        assert_eq!(config.session.compaction.target_ratio, 0.2);
    }

    #[test]
    fn contradictory_compaction_ratios_fall_back_as_a_group() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_path = tmp.path().join("config.toml");
        // Individually in range, but the trio cannot satisfy
        // target <= trigger <= emergency.
        std::fs::write(
            &project_path,
            "[session.compaction]\ntrigger_ratio = 0.9\nemergency_ratio = 0.2\ntarget_ratio = 0.95\n",
        )
        .unwrap();
        let config = CoreConfig::load_from_paths(None, None, Some(&project_path));
        assert_eq!(
            config.session.compaction.trigger_ratio,
            DEFAULT_TRIGGER_RATIO
        );
        assert_eq!(
            config.session.compaction.emergency_ratio,
            DEFAULT_EMERGENCY_RATIO
        );
        assert_eq!(config.session.compaction.target_ratio, DEFAULT_TARGET_RATIO);
    }

    #[test]
    fn layered_compaction_merges_fields_without_wholesale_replacement() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_path = tmp.path().join("user-config.toml");
        let project_path = tmp.path().join("project-config.toml");

        // The user layer disables compaction and pins a large window.
        std::fs::write(
            &user_path,
            "[session.compaction]\nenabled = false\nmodel_context_window = 200000\n",
        )
        .unwrap();
        // The higher-priority project layer touches only the target ratio.
        std::fs::write(&project_path, "[session.compaction]\ntarget_ratio = 0.2\n").unwrap();

        let config = CoreConfig::load_from_paths(None, Some(&user_path), Some(&project_path));
        let compaction = &config.session.compaction;
        // The project layer must not have reset the user-layer fields.
        assert!(!compaction.enabled);
        assert_eq!(compaction.model_context_window, Some(200_000));
        // The project layer's explicit field is applied.
        assert_eq!(compaction.target_ratio, 0.2);
        // Fields set by neither layer keep their compiled-in defaults.
        assert_eq!(compaction.trigger_ratio, DEFAULT_TRIGGER_RATIO);
        assert_eq!(compaction.emergency_ratio, DEFAULT_EMERGENCY_RATIO);
        assert!(compaction.auto);
    }

    #[test]
    fn higher_priority_layer_overrides_lower_compaction_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_path = tmp.path().join("user-config.toml");
        let project_path = tmp.path().join("project-config.toml");

        std::fs::write(
            &user_path,
            "[session.compaction]\npreserve_recent_runs = 5\nauto = true\n",
        )
        .unwrap();
        std::fs::write(
            &project_path,
            "[session.compaction]\npreserve_recent_runs = 9\n",
        )
        .unwrap();

        let config = CoreConfig::load_from_paths(None, Some(&user_path), Some(&project_path));
        let compaction = &config.session.compaction;
        // The project layer's explicit value wins over the user layer's.
        assert_eq!(compaction.preserve_recent_runs, 9);
        // The user-layer field the project layer omitted is preserved.
        assert!(compaction.auto);
    }

    #[test]
    fn parse_stdio_mcp_config() {
        let toml_str = r#"
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
timeout_ms = 5000
allowed_tools = ["read_file", "list_directory"]

[mcp.servers.filesystem.env]
API_KEY = "${FILESYSTEM_API_KEY}"
"#;

        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        let server = config.mcp.servers.get("filesystem").unwrap();
        assert_eq!(server.command, "npx");
        assert_eq!(server.timeout_ms, 5000);
        assert_eq!(server.startup_timeout_ms, 30_000);
        assert_eq!(server.allowed_tools.as_ref().unwrap().len(), 2);
        assert_eq!(server.env["API_KEY"], "${FILESYSTEM_API_KEY}");
    }

    #[test]
    fn parse_streamable_http_mcp_config() {
        let toml_str = r#"
[mcp.servers.remote]
url = "https://mcp.example.com/mcp"
bearer_token = "${MCP_TOKEN}"
allowed_tools = ["search"]
"#;

        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        let server = config.mcp.servers.get("remote").unwrap();
        assert_eq!(server.command, "");
        assert_eq!(server.url.as_deref(), Some("https://mcp.example.com/mcp"));
        assert_eq!(server.bearer_token.as_deref(), Some("${MCP_TOKEN}"));
    }

    /// `base_url` must never reach the SysOM endpoint, in any of the shapes
    /// that used to leak it there.
    ///
    /// Each shape is real: absent is a hand-written ECS RAM role profile; empty
    /// is what `cosh auth` writes for the aliyun template; a compat URL is what
    /// `settings.json` migration carries in regardless of provider type. All
    /// must leave `sysom_endpoint` empty so the resolver probes instead of
    /// signing SysOM traffic for an OpenAI-compatible host.
    #[test]
    fn base_url_never_becomes_the_sysom_endpoint() {
        for base_url_line in [
            "",
            r#"base_url = """#,
            r#"base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1""#,
            r#"base_url = "https://api.openai.com/v1""#,
        ] {
            let toml_str = format!(
                r#"
[ai]
active_provider = "aliyun-ecs"

[ai.providers.aliyun-ecs]
type = "aliyun"
auth_source = "ecs_ram_role"
{base_url_line}
model = "qwen3.7-plus"
"#
            );
            let config: CoreConfig = toml::from_str(&toml_str).unwrap();
            let resolved = config.resolve_provider();

            assert_eq!(
                resolved.sysom_endpoint, "",
                "base_url leaked into sysom_endpoint for {base_url_line:?}"
            );
        }
    }

    #[test]
    fn configured_sysom_endpoint_is_carried_through() {
        let toml_str = r#"
[ai]
active_provider = "aliyun-ecs"

[ai.providers.aliyun-ecs]
type = "aliyun"
auth_source = "ecs_ram_role"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
sysom_endpoint = "https://sysom.cn-shanghai.aliyuncs.com"
model = "qwen3.7-plus"
"#;
        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        let resolved = config.resolve_provider();

        assert_eq!(
            resolved.sysom_endpoint,
            "https://sysom.cn-shanghai.aliyuncs.com"
        );
        // The compat base_url stays intact for the OpenAI-compatible consumers.
        assert_eq!(
            resolved.base_url,
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
    }

    #[test]
    fn parse_ecs_ram_role_auth_source() {
        let toml_str = r#"
[ai]
active_provider = "aliyun-ecs"

[ai.providers.aliyun-ecs]
type = "aliyun"
auth_source = "ecs_ram_role"
model = "qwen3.7-plus"
"#;

        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        let provider = config.ai.providers.get("aliyun-ecs").unwrap();
        assert_eq!(provider.provider_type.as_deref(), Some("aliyun"));
        assert_eq!(provider.auth_source.as_deref(), Some("ecs_ram_role"));
        assert!(provider.access_key_id.is_none());
        assert!(provider.access_key_secret.is_none());
        assert!(provider.security_token.is_none());
    }

    #[test]
    fn explicit_ecs_ram_role_auth_source_does_not_need_static_credentials() {
        let toml_str = r#"
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
auth_source = "ecs_ram_role"
model = "qwen3.7-plus"
"#;

        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        let provider = config.ai.providers.get("aliyun").unwrap();
        assert_eq!(provider.auth_source.as_deref(), Some("ecs_ram_role"));
        assert!(provider.access_key_id.is_none());
        assert!(provider.access_key_secret.is_none());
        assert!(provider.security_token.is_none());
        assert_eq!(provider.model.as_deref(), Some("qwen3.7-plus"));
    }

    #[test]
    fn user_config_preserves_manual_aliyun_sts_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_path = tmp.path().join("user-config.toml");

        std::fs::write(
            &user_path,
            r#"
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "manual-ak"
access_key_secret = "manual-sk"
security_token = "manual-token"
"#,
        )
        .unwrap();

        let config = CoreConfig::load_from_paths(None, Some(&user_path), None);
        let resolved = config.resolve_provider();
        let persisted = std::fs::read_to_string(&user_path).unwrap();

        assert_eq!(resolved.auth_source, None);
        assert_eq!(resolved.access_key_id, "manual-ak");
        assert_eq!(resolved.access_key_secret, "manual-sk");
        assert_eq!(resolved.security_token.as_deref(), Some("manual-token"));
        assert!(persisted.contains("access_key_id = \"manual-ak\""));
        assert!(persisted.contains("access_key_secret = \"manual-sk\""));
        assert!(persisted.contains("security_token = \"manual-token\""));
    }

    #[test]
    fn resolve_provider_from_config() {
        let toml_str = r#"
[ai]
active_provider = "qwen"
active_model = "my-model"

[ai.providers.qwen]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "sk-test"
model = "qwen3-235b-a22b"
"#;
        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        let resolved = config.resolve_provider();
        assert_eq!(resolved.base_url, "https://example.com/v1");
        assert_eq!(resolved.api_key, "sk-test");
        assert_eq!(resolved.model, "my-model");
    }

    #[test]
    fn resolve_provider_reads_explicit_cache() {
        let toml_str = r#"
[ai]
active_provider = "dashscope"

[ai.providers.dashscope]
type = "dashscope"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = "sk-test"
model = "qwen-max"
explicit_cache = true
"#;
        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        let resolved = config.resolve_provider();
        assert!(resolved.explicit_cache);
    }

    #[test]
    fn resolve_provider_defaults_explicit_cache_to_false() {
        let toml_str = r#"
[ai]
active_provider = "dashscope"

[ai.providers.dashscope]
type = "dashscope"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = "sk-test"
model = "qwen-max"
"#;
        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        let resolved = config.resolve_provider();
        assert!(!resolved.explicit_cache);
    }

    #[test]
    fn expand_env_vars_in_api_key() {
        std::env::set_var("TEST_COSH_KEY", "sk-from-env");
        let result = expand_env_vars("${TEST_COSH_KEY}");
        assert_eq!(result, "sk-from-env");
        std::env::remove_var("TEST_COSH_KEY");
    }

    #[test]
    fn expand_env_vars_no_match() {
        let result = expand_env_vars("plain-text");
        assert_eq!(result, "plain-text");
    }

    #[test]
    fn partial_config_uses_defaults() {
        let toml_str = r#"
[ai]
active_model = "test-model"
"#;
        let config: CoreConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.agent.approval_mode, ApprovalMode::Recommend);
        assert_eq!(config.agent.max_turns, 50);
        assert!(config.ai.providers.is_empty());
    }

    #[test]
    fn env_overrides() {
        // All env var tests in one function to avoid parallel race conditions.
        // Phase 1: valid overrides
        std::env::set_var("COSH_APPROVAL_MODE", "trust");
        std::env::set_var("COSH_MODEL", "gpt-4");
        // Differs from the built-in default so the assertion proves the
        // override took effect rather than matching it by coincidence.
        std::env::set_var("COSH_MAX_TURNS", "75");
        std::env::set_var("COSH_OUTPUT_LANGUAGE", "zh-CN");

        let mut config = CoreConfig::default();
        config.apply_env_overrides();

        assert_eq!(config.agent.approval_mode, ApprovalMode::Trust);
        assert_eq!(config.ai.active_model.as_deref(), Some("gpt-4"));
        assert_eq!(config.agent.max_turns, 75);
        assert_eq!(config.ai.output_language.as_deref(), Some("zh-CN"));

        // Legacy aliases are accepted but are normalized before use.
        std::env::set_var("COSH_APPROVAL_MODE", "balanced");
        let mut alias_config = CoreConfig::default();
        alias_config.apply_env_overrides();
        assert_eq!(alias_config.agent.approval_mode, ApprovalMode::Recommend);

        // An invalid override must tighten a previously trusted config.
        std::env::set_var("COSH_APPROVAL_MODE", "invalid");
        let mut invalid_config = CoreConfig::default();
        invalid_config.agent.approval_mode = ApprovalMode::Trust;
        invalid_config.apply_env_overrides();
        assert_eq!(invalid_config.agent.approval_mode, ApprovalMode::Recommend);

        // Phase 2: invalid max_turns — should be ignored
        std::env::set_var("COSH_MAX_TURNS", "not-a-number");
        let mut config2 = CoreConfig::default();
        config2.apply_env_overrides();
        assert_eq!(config2.agent.max_turns, 50);

        // Cleanup
        std::env::remove_var("COSH_APPROVAL_MODE");
        std::env::remove_var("COSH_MODEL");
        std::env::remove_var("COSH_MAX_TURNS");
        std::env::remove_var("COSH_OUTPUT_LANGUAGE");
    }

    #[test]
    fn layered_config_preserves_user_provider_and_project_model() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_path = tmp.path().join("user-config.toml");
        let project_path = tmp.path().join("project-config.toml");

        std::fs::write(
            &user_path,
            r#"
[ai]
active_provider = "dashscope"
active_model = "user-model"

[ai.providers.dashscope]
type = "dashscope"
api_key = "sk-user"
model = "provider-model"
"#,
        )
        .unwrap();
        std::fs::write(
            &project_path,
            r#"
[ai]
active_model = "project-model"
"#,
        )
        .unwrap();

        let config = CoreConfig::load_from_paths(None, Some(&user_path), Some(&project_path));
        let resolved = config.resolve_provider();

        assert_eq!(config.ai.active_provider.as_deref(), Some("dashscope"));
        assert_eq!(config.ai.active_model.as_deref(), Some("project-model"));
        assert_eq!(resolved.api_key, "sk-user");
        assert_eq!(resolved.model, "project-model");
    }

    #[test]
    fn bare_config_preserves_user_provider_but_ignores_project_and_runtime_extensions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_path = tmp.path().join("user-config.toml");
        let project_path = tmp.path().join("project-config.toml");
        std::fs::write(
            &user_path,
            r#"
[ai]
active_provider = "user-provider"
active_model = "user-model"

[ai.providers.user-provider]
api_key = "sk-user"

[hooks]
enabled = true

[skills]
enabled = true
custom_paths = ["/user/skills"]
"#,
        )
        .unwrap();
        std::fs::write(
            &project_path,
            r#"
[ai]
active_model = "project-model"
"#,
        )
        .unwrap();

        let mut config = CoreConfig::load_from_paths(None, Some(&user_path), None);
        config.apply_bare_isolation();

        assert_eq!(config.ai.active_provider.as_deref(), Some("user-provider"));
        assert_eq!(config.ai.active_model.as_deref(), Some("user-model"));
        assert_eq!(config.resolve_provider().api_key, "sk-user");
        assert!(!config.hooks.enabled);
        assert_eq!(config.hooks.enabled_override, Some(false));
        assert!(!config.skills.enabled);
        assert!(config.skills.custom_paths.is_empty());
        assert!(!config.session.auto_persist);
        assert!(project_path.exists());
    }

    #[test]
    fn project_auth_fields_are_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_path = tmp.path().join("project-config.toml");

        std::fs::write(
            &project_path,
            r#"
[ai]
active_provider = "project-provider"
active_model = "project-model"

[ai.providers.project-provider]
type = "dashscope"
api_key = "sk-project"
auth_source = "ecs_ram_role"
"#,
        )
        .unwrap();

        let config = CoreConfig::load_from_paths(None, None, Some(&project_path));
        assert!(config.ai.active_provider.is_none());
        assert!(config.ai.providers.is_empty());
        assert_eq!(config.ai.active_model.as_deref(), Some("project-model"));
    }

    #[test]
    fn project_mcp_config_is_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_path = tmp.path().join("user-config.toml");
        let project_path = tmp.path().join("project-config.toml");
        std::fs::write(
            &user_path,
            r#"
[mcp.servers.user]
command = "user-server"
"#,
        )
        .unwrap();
        std::fs::write(
            &project_path,
            r#"
[mcp.servers.untrusted]
command = "project-server"
"#,
        )
        .unwrap();

        let config = CoreConfig::load_from_paths(None, Some(&user_path), Some(&project_path));
        assert!(config.mcp.servers.contains_key("user"));
        assert!(!config.mcp.servers.contains_key("untrusted"));
    }

    #[test]
    fn user_provider_overrides_system_provider_atomically() {
        let tmp = tempfile::TempDir::new().unwrap();
        let system_path = tmp.path().join("system-config.toml");
        let user_path = tmp.path().join("user-config.toml");

        std::fs::write(
            &system_path,
            r#"
[ai]
active_provider = "shared"

[ai.providers.shared]
type = "dashscope"
base_url = "https://system.example/v1"
api_key = "sk-system"
model = "system-model"
"#,
        )
        .unwrap();
        std::fs::write(
            &user_path,
            r#"
[ai]
active_provider = "shared"

[ai.providers.shared]
type = "openai_compat"
api_key = "sk-user"
"#,
        )
        .unwrap();

        let config = CoreConfig::load_from_paths(Some(&system_path), Some(&user_path), None);
        let provider = config.ai.providers.get("shared").unwrap();

        assert_eq!(provider.provider_type.as_deref(), Some("openai_compat"));
        assert_eq!(provider.api_key.as_deref(), Some("sk-user"));
        assert!(provider.base_url.is_none());
        assert!(provider.model.is_none());
    }

    #[test]
    fn sysom_endpoint_survives_refresh_persist_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[ai]
active_provider = "aliyun"
[ai.providers.aliyun]
type = "aliyun"
sysom_endpoint = "https://sysom.cn-shanghai.aliyuncs.com"
"#,
        )
        .unwrap();
        let mut config = CoreConfig::load_from_paths(None, Some(&path), None);
        let response = crate::auth::AuthResponse {
            provider_id: "aliyun".to_string(),
            provider_type: None,
            values: HashMap::from([
                ("access_key_id".to_string(), "new-ak".to_string()),
                ("access_key_secret".to_string(), "new-sk".to_string()),
            ]),
            persist: true,
        };
        crate::auth::apply_auth_credentials(&mut config, &response).unwrap();
        persist_config_to_dir(&config, tmp.path()).unwrap();
        let reloaded = CoreConfig::load_from_paths(None, Some(&path), None).resolve_provider();
        assert_eq!(
            reloaded.sysom_endpoint,
            "https://sysom.cn-shanghai.aliyuncs.com"
        );
        assert_eq!(reloaded.access_key_id, "new-ak");
    }

    #[test]
    fn persist_user_config_does_not_write_project_overrides() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_dir = tmp.path().join("home-config");
        let user_path = user_dir.join("config.toml");
        let project_path = tmp.path().join("project-config.toml");
        std::fs::create_dir_all(&user_dir).unwrap();

        std::fs::write(
            &user_path,
            r#"
[ai]
active_provider = "dashscope"
active_model = "user-model"
output_language = "en-US"

[ai.providers.dashscope]
type = "dashscope"
api_key = "sk-user"
"#,
        )
        .unwrap();
        std::fs::write(
            &project_path,
            r#"
[ai]
active_model = "project-model"
output_language = "zh-CN"
"#,
        )
        .unwrap();

        let mut config = CoreConfig::load_from_paths(None, Some(&user_path), Some(&project_path));
        config.ai.active_provider = Some("dashscope".to_string());
        persist_config_to_dir(&config, &user_dir).unwrap();

        let content = std::fs::read_to_string(&user_path).unwrap();
        assert!(content.contains("active_model = \"user-model\""));
        assert!(content.contains("output_language = \"en-US\""));
        assert!(!content.contains("project-model"));
        assert!(!content.contains("zh-CN"));
        assert!(content.contains("api_key = \"sk-user\""));
    }

    #[test]
    fn persist_user_config_does_not_write_system_providers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let system_path = tmp.path().join("system-config.toml");
        let user_dir = tmp.path().join("home-config");
        let user_path = user_dir.join("config.toml");
        std::fs::create_dir_all(&user_dir).unwrap();

        std::fs::write(
            &system_path,
            r#"
[ai]
active_provider = "system-provider"

[ai.providers.system-provider]
type = "dashscope"
api_key = "sk-system"
model = "system-model"
"#,
        )
        .unwrap();
        std::fs::write(
            &user_path,
            r#"
[ai]
active_provider = "user-provider"

[ai.providers.user-provider]
type = "dashscope"
api_key = "sk-user"
"#,
        )
        .unwrap();

        let mut config = CoreConfig::load_from_paths(Some(&system_path), Some(&user_path), None);
        config.ai.active_provider = Some("system-provider".to_string());
        persist_config_to_dir(&config, &user_dir).unwrap();

        let content = std::fs::read_to_string(&user_path).unwrap();
        assert!(content.contains("active_provider = \"user-provider\""));
        assert!(content.contains("[ai.providers.user-provider]"));
        assert!(content.contains("api_key = \"sk-user\""));
        assert!(!content.contains("system-provider"));
        assert!(!content.contains("sk-system"));
    }

    #[test]
    fn persist_preserves_non_ai_sections_whose_names_start_with_ai() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_dir = tmp.path().join("home-config");
        let user_path = user_dir.join("config.toml");
        std::fs::create_dir_all(&user_dir).unwrap();

        std::fs::write(
            &user_path,
            r#"[ai]
active_provider = "old"

[ai.providers.old]
type = "openai_compat"
base_url = "https://old.example/v1"
api_key = "sk-old"
model = "old-model"

[aider]
model = "claude-3-5-sonnet"
edit_format = "diff"

[aide]
theme = "dark"

[ai_drivers]
enabled = true

[agent]
approval_mode = "balanced"
"#,
        )
        .unwrap();

        let mut config = CoreConfig::load_from_paths(None, Some(&user_path), None);
        config.user_ai.active_provider = Some("new-provider".to_string());
        config.user_ai.providers.clear();
        let provider = ProviderConfig {
            provider_type: Some("openai_compat".to_string()),
            base_url: Some("https://new.example/v1".to_string()),
            api_key: Some("sk-new".to_string()),
            model: Some("new-model".to_string()),
            ..Default::default()
        };
        config
            .user_ai
            .providers
            .insert("new-provider".to_string(), provider);

        persist_config_to_dir(&config, &user_dir).unwrap();

        let content = std::fs::read_to_string(&user_path).unwrap();

        // Non-ai sections with ai-prefixed names must survive.
        assert!(content.contains("[aider]"));
        assert!(content.contains("model = \"claude-3-5-sonnet\""));
        assert!(content.contains("edit_format = \"diff\""));
        assert!(content.contains("[aide]"));
        assert!(content.contains("theme = \"dark\""));
        assert!(content.contains("[ai_drivers]"));
        assert!(content.contains("enabled = true"));
        assert!(content.contains("[agent]"));
        assert!(content.contains("approval_mode = \"balanced\""));

        // Old ai content must be gone, new content present exactly once.
        assert!(!content.contains("sk-old"));
        assert!(!content.contains("old-model"));
        assert!(!content.contains("[ai.providers.old]"));
        assert!(content.contains("active_provider = \"new-provider\""));
        assert!(content.contains("[ai.providers.new-provider]"));
        assert!(content.contains("api_key = \"sk-new\""));
        assert_eq!(content.matches("[ai]").count(), 1);
        assert_eq!(content.matches("[ai.providers.").count(), 1);
    }

    #[test]
    fn persist_replaces_ai_headers_with_inline_comments_without_duplication() {
        let tmp = tempfile::TempDir::new().unwrap();
        let user_dir = tmp.path().join("home-config");
        let user_path = user_dir.join("config.toml");
        std::fs::create_dir_all(&user_dir).unwrap();

        std::fs::write(
            &user_path,
            r#"[ai] # valid inline comment
active_provider = "old"

[ai.providers.old] # legacy provider
type = "openai_compat"
api_key = "sk-old"

[aider]
model = "claude-3-5-sonnet"

[agent]
approval_mode = "balanced"
"#,
        )
        .unwrap();

        let mut config = CoreConfig::load_from_paths(None, Some(&user_path), None);
        config.user_ai.active_provider = Some("new-provider".to_string());
        config.user_ai.providers.clear();
        let provider = ProviderConfig {
            provider_type: Some("openai_compat".to_string()),
            api_key: Some("sk-new".to_string()),
            ..Default::default()
        };
        config
            .user_ai
            .providers
            .insert("new-provider".to_string(), provider);

        persist_config_to_dir(&config, &user_dir).unwrap();

        let content = std::fs::read_to_string(&user_path).unwrap();

        // No duplicate [ai] table; old content fully replaced.
        assert_eq!(content.matches("[ai]").count(), 1);
        assert!(!content.contains("sk-old"));
        assert!(!content.contains("[ai.providers.old]"));
        assert!(content.contains("active_provider = \"new-provider\""));
        assert!(content.contains("[ai.providers.new-provider]"));

        // Non-ai sections survive.
        assert!(content.contains("[aider]"));
        assert!(content.contains("model = \"claude-3-5-sonnet\""));
        assert!(content.contains("[agent]"));

        // Persisted output must be valid TOML that re-parses cleanly.
        let parsed: CoreConfig = toml::from_str(&content).unwrap();
        assert_eq!(parsed.ai.active_provider.as_deref(), Some("new-provider"));
        assert!(parsed.ai.providers.contains_key("new-provider"));
        assert_eq!(parsed.ai.providers.len(), 1);
    }
}
