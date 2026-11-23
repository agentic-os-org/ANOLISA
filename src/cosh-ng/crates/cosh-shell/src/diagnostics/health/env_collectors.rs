//! On-demand environment health collectors: provider readiness,
//! configuration, hooks, PTY support, and permissions.
//!
//! Cross-platform, synchronous, side-effect free, and infallible. Each
//! collector records facts, marks its check as done, and attaches a `Warning`
//! finding when something needs attention. One collector failing never blocks
//! the others.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{project_hook_trust, CoshConfig, ProjectHookTrust};

use super::builder::HealthReportBuilder;
use super::model::{
    HealthCollector, HealthFactCategory, HealthFactSource, HealthFactValue, HealthFinding,
    HealthFindingCategory, HealthMessageId, HealthSeverity, HealthUnavailableReason,
};

mod permissions;
use permissions::collect_permissions;
#[cfg(all(test, target_os = "linux"))]
use permissions::open_anonymous_probe;
pub(crate) use permissions::{classify_permissions, PermissionsReadiness};

/// Run every environment collector into `builder`. Collectors are independent;
/// each contributes facts/findings without depending on the others.
#[cfg(test)]
fn run_env_collectors(
    builder: &mut HealthReportBuilder,
    config: &CoshConfig,
    cwd: &Path,
    elapsed_ms: u128,
) {
    let config_sample = OnceCell::new();
    for collector in [
        HealthCollector::Provider,
        HealthCollector::Config,
        HealthCollector::Hooks,
        HealthCollector::Pty,
        HealthCollector::Permissions,
    ] {
        run_environment_check(
            builder,
            collector,
            config,
            cwd,
            elapsed_ms,
            true,
            &config_sample,
        );
    }
}

pub(super) fn run_environment_check(
    builder: &mut HealthReportBuilder,
    collector: HealthCollector,
    config: &CoshConfig,
    cwd: &Path,
    elapsed_ms: u128,
    allow_write_probe: bool,
    config_sample: &OnceCell<EnvironmentConfigSample>,
) {
    match collector {
        HealthCollector::Provider => collect_provider(builder, config, elapsed_ms, config_sample),
        HealthCollector::Config => collect_config(
            builder,
            config,
            elapsed_ms,
            config_sample.get_or_init(EnvironmentConfigSample::read),
        ),
        HealthCollector::Hooks => collect_hooks(builder, config, cwd, elapsed_ms),
        HealthCollector::Pty => collect_pty(builder, elapsed_ms),
        HealthCollector::Permissions if allow_write_probe => {
            collect_permissions(builder, elapsed_ms)
        }
        HealthCollector::Permissions => {
            builder.declare_check(collector, false);
        }
        _ => {}
    }
}

fn env_finding(
    id: &str,
    title_id: HealthMessageId,
    detail_id: HealthMessageId,
    detail_args: BTreeMap<String, String>,
    evidence_fact_ids: Vec<String>,
) -> HealthFinding {
    HealthFinding {
        id: id.to_string(),
        severity: HealthSeverity::Warning,
        category: HealthFindingCategory::Observation,
        title_id,
        detail_id: Some(detail_id),
        detail_args,
        evidence_fact_ids,
        suggested_try_ids: Vec::new(),
    }
}

// ─── Provider readiness (static, no network) ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderReadiness {
    Ready,
    MissingCredentials,
    UnknownAdapter,
}

/// Pure classification of provider readiness. Unknown adapters are never
/// `Ready`, even if generic credentials happen to exist.
pub(crate) fn classify_provider(adapter: &str, has_credentials: bool) -> ProviderReadiness {
    match crate::adapter::AdapterKind::parse(adapter) {
        None => ProviderReadiness::UnknownAdapter,
        Some(crate::adapter::AdapterKind::Fake) => ProviderReadiness::Ready,
        Some(_) => {
            if has_credentials {
                ProviderReadiness::Ready
            } else {
                ProviderReadiness::MissingCredentials
            }
        }
    }
}

fn env_non_empty(key: &str) -> bool {
    std::env::var(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

/// Expand a config value that may reference an env var (`${VAR}` / `$VAR`),
/// mirroring cosh-core's `expand_env_vars`.
fn config_value_present(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    let resolved = if let Some(var) = trimmed
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
    {
        std::env::var(var).unwrap_or_default()
    } else if let Some(var) = trimmed.strip_prefix('$') {
        std::env::var(var).unwrap_or_default()
    } else {
        trimmed.to_string()
    };
    !resolved.trim().is_empty()
}

/// Whether env credentials satisfy the adapter, gated by active provider type.
/// cosh-core's `aliyun` provider ignores generic API-key env vars.
fn env_credentials_present(kind: crate::adapter::AdapterKind, provider_type: &str) -> bool {
    use crate::adapter::AdapterKind;
    match kind {
        AdapterKind::Fake => true,
        AdapterKind::CoshCore => match provider_type {
            "aliyun" => {
                env_non_empty("ALIBABA_CLOUD_ACCESS_KEY_ID")
                    && env_non_empty("ALIBABA_CLOUD_ACCESS_KEY_SECRET")
            }
            "mock" => true,
            _ => env_non_empty("DASHSCOPE_API_KEY") || env_non_empty("OPENAI_API_KEY"),
        },
        AdapterKind::ClaudeCode => env_non_empty("ANTHROPIC_API_KEY"),
        AdapterKind::QwenCli => env_non_empty("DASHSCOPE_API_KEY"),
    }
}

/// Whether the adapter's credentials are configured in
/// `~/.copilot-shell/config.toml`, mirroring how cosh-core resolves provider
/// config.  cosh-core reads only `[ai].active_provider` (defaulting to
/// `"default"`) and looks up that single entry — credentials on a non-active
/// provider are invisible to it, so the doctor must not count them either.
fn config_credentials_present(
    kind: crate::adapter::AdapterKind,
    provider: Option<&toml::value::Table>,
) -> bool {
    use crate::adapter::AdapterKind;
    let Some(provider) = provider else {
        return false;
    };
    let field = |key: &str| provider.get(key).and_then(toml::Value::as_str);
    // cosh-core resolves `type` (serialized via #[serde(rename = "type")],
    // default "generic") from the active provider entry and branches: "aliyun"
    // uses AK/SK or ECS RAM role only; "mock" needs nothing; everything else
    // uses api_key.
    let provider_type = field("type").unwrap_or("generic");
    match kind {
        AdapterKind::Fake => true,
        AdapterKind::CoshCore => match provider_type {
            "aliyun" => {
                field("auth_source") == Some("ecs_ram_role")
                    || (field("access_key_id").is_some_and(config_value_present)
                        && field("access_key_secret").is_some_and(config_value_present))
            }
            "mock" => true,
            _ => field("api_key").is_some_and(config_value_present),
        },
        AdapterKind::ClaudeCode | AdapterKind::QwenCli => {
            field("api_key").is_some_and(config_value_present)
        }
    }
}

fn provider_credentials_from_sample(
    adapter: &str,
    sample: &OnceCell<EnvironmentConfigSample>,
) -> bool {
    let Some(kind) = crate::adapter::AdapterKind::parse(adapter) else {
        return false;
    };
    if kind == crate::adapter::AdapterKind::Fake {
        return true;
    }
    let sample = sample.get_or_init(EnvironmentConfigSample::read);
    let active = std::env::var("COSH_AI_PROVIDER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            sample
                .value
                .as_ref()?
                .get("ai")?
                .get("active_provider")?
                .as_str()
                .map(String::from)
        })
        .unwrap_or_else(|| "default".into());
    let provider = sample
        .value
        .as_ref()
        .and_then(|value| value.get("ai")?.get("providers")?.get(&active)?.as_table());
    let provider_type = provider
        .and_then(|value| value.get("type")?.as_str())
        .unwrap_or("generic");
    env_credentials_present(kind, provider_type) || config_credentials_present(kind, provider)
}

#[cfg(test)]
fn provider_credentials_present(adapter: &str) -> bool {
    provider_credentials_from_sample(adapter, &OnceCell::new())
}

fn collect_provider(
    builder: &mut HealthReportBuilder,
    config: &CoshConfig,
    elapsed_ms: u128,
    sample: &OnceCell<EnvironmentConfigSample>,
) {
    let adapter = config.adapter_default.trim();
    let has_credentials = provider_credentials_from_sample(adapter, sample);
    builder
        .add_fact(
            HealthFactCategory::Provider,
            "provider.adapter",
            HealthFactValue::String(adapter.to_string()),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Provider,
            "provider.credentials_present",
            HealthFactValue::Bool(has_credentials),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_check_done("provider");

    let readiness = classify_provider(adapter, has_credentials);
    if readiness != ProviderReadiness::Ready {
        let mut args = BTreeMap::new();
        args.insert(
            "adapter".to_string(),
            if adapter.is_empty() {
                "unknown".to_string()
            } else {
                adapter.to_string()
            },
        );
        // Unknown adapters need a different nudge: the user must pick a valid
        // adapter name, not configure credentials for one that does not exist.
        let remediation = match readiness {
            ProviderReadiness::UnknownAdapter => HealthMessageId::HealthRemediationUnknownAdapter,
            _ => HealthMessageId::HealthRemediationProvider,
        };
        builder.add_finding(env_finding(
            "env-provider",
            HealthMessageId::HealthFindingProviderUnconfigured,
            remediation,
            args,
            vec!["provider.adapter".to_string()],
        ));
    }
}

// ─── Configuration ────────────────────────────────────────────────────────

/// Outcome of trying to consume `~/.copilot-shell/config.toml`. `read` and
/// `parse` are tracked separately so the doctor surfaces both an unreadable
/// file (permissions/directory) and a readable-but-invalid TOML file, instead
/// of relying on load_config()'s silent fallback to defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigFileStatus {
    pub(crate) readable: bool,
    pub(crate) parseable: bool,
}

impl ConfigFileStatus {
    fn consumable() -> Self {
        Self {
            readable: true,
            parseable: true,
        }
    }
}

pub(super) struct EnvironmentConfigSample {
    value: Option<toml::Value>,
    status: ConfigFileStatus,
}

impl EnvironmentConfigSample {
    pub(super) fn read() -> Self {
        let path = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".copilot-shell/config.toml"));
        let content = path.map(std::fs::read_to_string);
        match content {
            None => Self {
                value: None,
                status: ConfigFileStatus::consumable(),
            },
            Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Self {
                value: None,
                status: ConfigFileStatus::consumable(),
            },
            Some(Err(_)) => Self {
                value: None,
                status: ConfigFileStatus {
                    readable: false,
                    parseable: false,
                },
            },
            Some(Ok(content)) => {
                let value = content.parse::<toml::Value>().ok();
                let parseable = value.is_some() || config_content_consumable(&content);
                Self {
                    value,
                    status: ConfigFileStatus {
                        readable: true,
                        parseable,
                    },
                }
            }
        }
    }
}

#[cfg(test)]
fn config_file_status() -> ConfigFileStatus {
    EnvironmentConfigSample::read().status
}

/// Recognized keys of the legacy simple `key = value` config parser
/// (`config/parse.rs::parse_simple_config`). Kept in sync so the doctor does
/// not flag legacy configs the shell still loads.
const SIMPLE_CONFIG_KEYS: &[&str] = &[
    "shell.default",
    "shell.analysis_mode",
    "shell.approval_mode",
    "shell.adapter_default",
    "shell.trusted_command",
    "shell.trusted_project_root",
    "ui.language",
    "ui.startup_banner",
    "ui.startup_hooks",
    "ui.debug",
    "ui.log_level",
    "logging.level",
    "health.enabled",
    "health.role",
    "health.memory_sensitive",
    "health.verbose",
];

fn config_content_consumable(content: &str) -> bool {
    let mut has_meaningful_line = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        has_meaningful_line = true;
        if let Some((key, _)) = line.split_once('=') {
            if SIMPLE_CONFIG_KEYS.contains(&key.trim()) {
                // The legacy simple parser recognizes at least one key.
                return true;
            }
        }
    }
    // Only comments/blank lines is fine; genuinely unusable content is not.
    !has_meaningful_line
}

fn collect_config(
    builder: &mut HealthReportBuilder,
    config: &CoshConfig,
    elapsed_ms: u128,
    sample: &EnvironmentConfigSample,
) {
    let home_present = std::env::var_os("HOME")
        .map(|home| !home.is_empty())
        .unwrap_or(false);
    let status = sample.status;
    builder
        .add_fact(
            HealthFactCategory::Config,
            "config.home_present",
            HealthFactValue::Bool(home_present),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Config,
            "config.readable",
            HealthFactValue::Bool(status.readable),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Config,
            "config.parseable",
            HealthFactValue::Bool(status.parseable),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Config,
            "config.language",
            HealthFactValue::String(config.language.clone()),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Config,
            "config.adapter_default",
            HealthFactValue::String(config.adapter_default.clone()),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Config,
            "config.approval_mode",
            HealthFactValue::String(config.approval_mode.label().to_string()),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_check_done("config");

    // Distinct remediation per failure so the guidance points at the real fix:
    // missing HOME, an unreadable file/directory, or content the shell cannot
    // consume (neither valid TOML nor recognized legacy config).
    if !home_present {
        builder.add_finding(env_finding(
            "env-config",
            HealthMessageId::HealthFindingConfigUnavailable,
            HealthMessageId::HealthRemediationConfig,
            BTreeMap::new(),
            vec!["config.home_present".to_string()],
        ));
    } else if !status.readable {
        builder.add_finding(env_finding(
            "env-config",
            HealthMessageId::HealthFindingConfigUnavailable,
            HealthMessageId::HealthRemediationConfigUnreadable,
            BTreeMap::new(),
            vec!["config.readable".to_string()],
        ));
    } else if !status.parseable {
        builder.add_finding(env_finding(
            "env-config",
            HealthMessageId::HealthFindingConfigUnavailable,
            HealthMessageId::HealthRemediationConfigInvalid,
            BTreeMap::new(),
            vec!["config.parseable".to_string()],
        ));
    }
}

// ─── Hooks ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HooksReadiness {
    Ok,
    ProjectUntrusted,
}

/// Pure classification for hook readiness. No I/O.
pub(crate) fn classify_hooks(project_present: bool, project_trusted: bool) -> HooksReadiness {
    if project_present && !project_trusted {
        HooksReadiness::ProjectUntrusted
    } else {
        HooksReadiness::Ok
    }
}

fn collect_hooks(
    builder: &mut HealthReportBuilder,
    config: &CoshConfig,
    cwd: &Path,
    elapsed_ms: u128,
) {
    let user_dir_present = user_hooks_dir_present();
    let project_root = project_hook_root(cwd);
    let project_present = project_root.is_some();
    let project_trusted = match &project_root {
        Some(root) => {
            project_hook_trust(
                root,
                &config.trusted_project_roots,
                &config.consented_project_hooks,
                crate::hooks::HookEngine::project_hooks_pinned_digest_for_root(root).as_deref(),
            ) == ProjectHookTrust::Trusted
        }
        None => true,
    };
    builder
        .add_fact(
            HealthFactCategory::Hooks,
            "hooks.user_dir_present",
            HealthFactValue::Bool(user_dir_present),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Hooks,
            "hooks.project_present",
            HealthFactValue::Bool(project_present),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Hooks,
            "hooks.project_trusted",
            HealthFactValue::Bool(project_trusted),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_check_done("hooks");

    if classify_hooks(project_present, project_trusted) == HooksReadiness::ProjectUntrusted {
        let mut args = BTreeMap::new();
        args.insert(
            "path".to_string(),
            project_root
                .as_ref()
                .map(|root| root.display().to_string())
                .unwrap_or_default(),
        );
        builder.add_finding(env_finding(
            "env-hooks",
            HealthMessageId::HealthFindingHooksUntrusted,
            HealthMessageId::HealthRemediationHooks,
            args,
            vec!["hooks.project_trusted".to_string()],
        ));
    }
}

/// Local reimplementations of the hook path probes so the diagnostics engine
/// stays independent of the binary-only `hooks` module facade.
fn user_hooks_dir_present() -> bool {
    std::env::var_os("HOME")
        .map(|home| {
            PathBuf::from(home)
                .join(".copilot-shell/cosh/hooks")
                .is_dir()
        })
        .unwrap_or(false)
}

fn project_hook_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|candidate| candidate.join(".cosh/hooks").is_dir())
        .map(canonical_root)
}

fn canonical_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

// ─── PTY support ──────────────────────────────────────────────────────────

fn pty_available() -> bool {
    // Static, side-effect-free probe: presence of the PTY multiplexer device.
    Path::new("/dev/ptmx").exists()
}

fn collect_pty(builder: &mut HealthReportBuilder, elapsed_ms: u128) {
    let available = pty_available();
    builder
        .add_fact(
            HealthFactCategory::Pty,
            "pty.ptmx_available",
            HealthFactValue::Bool(available),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_check_done("pty");

    if !available {
        builder.add_finding(env_finding(
            "env-pty",
            HealthMessageId::HealthFindingPtyUnavailable,
            HealthMessageId::HealthRemediationPty,
            BTreeMap::new(),
            vec!["pty.ptmx_available".to_string()],
        ));
    }
}

// ─── Permissions ──────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "env_collector_tests.rs"]
mod env_collector_tests;
