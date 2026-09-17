//! On-demand environment health collectors: provider readiness,
//! configuration, hooks, PTY support, permissions, runtime process state,
//! and runtime log evidence.
//!
//! Cross-platform, synchronous, side-effect free, and infallible. Each
//! collector records facts, marks its check as done, and attaches a `Warning`
//! finding when something needs attention. One collector failing never blocks
//! the others.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::CoshConfig;
use crate::diagnostics::run_registry::RunEntry;

use super::builder::HealthReportBuilder;
use super::model::{
    HealthFactCategory, HealthFactSource, HealthFactValue, HealthFinding, HealthFindingCategory,
    HealthMessageId, HealthSeverity,
};

/// Run every environment collector into `builder`. Collectors are independent;
/// each contributes facts/findings without depending on the others.
pub(crate) fn run_env_collectors(
    builder: &mut HealthReportBuilder,
    config: &CoshConfig,
    cwd: &Path,
    elapsed_ms: u128,
) {
    collect_provider(builder, config, elapsed_ms);
    collect_config(builder, config, elapsed_ms);
    collect_hooks(builder, config, cwd, elapsed_ms);
    collect_pty(builder, elapsed_ms);
    collect_permissions(builder, elapsed_ms);
    collect_runtime(builder, elapsed_ms);
    collect_logs(builder, elapsed_ms);
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
fn config_credentials_present(kind: crate::adapter::AdapterKind) -> bool {
    use crate::adapter::AdapterKind;
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let path = PathBuf::from(home).join(".copilot-shell/config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(value) = content.parse::<toml::Value>() else {
        return false;
    };
    // Resolve the active provider name, mirroring cosh-core's
    // `resolve_provider()`: `COSH_AI_PROVIDER` env var overrides, then
    // `[ai].active_provider`, default to "default".
    let active_provider = std::env::var("COSH_AI_PROVIDER")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            value
                .get("ai")
                .and_then(|ai| ai.get("active_provider"))
                .and_then(toml::Value::as_str)
                .map(String::from)
        })
        .unwrap_or_else(|| "default".to_string());
    let Some(providers) = value
        .get("ai")
        .and_then(|ai| ai.get("providers"))
        .and_then(toml::Value::as_table)
    else {
        return false;
    };
    // Check only the active provider entry — non-active entries are ignored by
    // cosh-core and must not satisfy the doctor either.
    let Some(provider) = providers.get(active_provider.as_str()) else {
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

fn provider_credentials_present(adapter: &str) -> bool {
    let Some(kind) = crate::adapter::AdapterKind::parse(adapter) else {
        return false;
    };
    if kind == crate::adapter::AdapterKind::Fake {
        return true;
    }
    // Resolve the active provider type from config so both env and config
    // credential checks are gated by it.  cosh-core reads `provider_type`
    // (default "generic") and branches: "aliyun" ignores api_key / generic
    // API-key env vars, so env credentials alone must not satisfy an aliyun
    // active provider.
    let provider_type = resolve_active_provider_type();
    env_credentials_present(kind, &provider_type) || config_credentials_present(kind)
}

/// Read `[ai].active_provider` (default `"default"`), look up that entry in
/// `[ai.providers.*]`, and return its `provider_type` (default `"generic"`).
/// When no config file is readable, falls back to `"generic"`.
fn resolve_active_provider_type() -> String {
    let home = match std::env::var_os("HOME") {
        Some(h) => h,
        None => return "generic".to_string(),
    };
    let path = PathBuf::from(home).join(".copilot-shell/config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return "generic".to_string();
    };
    let Ok(value) = content.parse::<toml::Value>() else {
        return "generic".to_string();
    };
    let active = std::env::var("COSH_AI_PROVIDER")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            value
                .get("ai")
                .and_then(|ai| ai.get("active_provider"))
                .and_then(toml::Value::as_str)
                .map(String::from)
        })
        .unwrap_or_else(|| "default".to_string());
    value
        .get("ai")
        .and_then(|ai| ai.get("providers"))
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get(&active))
        .and_then(|p| p.get("type"))
        .and_then(toml::Value::as_str)
        .unwrap_or("generic")
        .to_string()
}

fn collect_provider(builder: &mut HealthReportBuilder, config: &CoshConfig, elapsed_ms: u128) {
    let adapter = config.adapter_default.trim();
    let has_credentials = provider_credentials_present(adapter);
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

fn config_file_status() -> ConfigFileStatus {
    let Some(home) = std::env::var_os("HOME") else {
        // No HOME is reported through the separate home_present check.
        return ConfigFileStatus::consumable();
    };
    let path = PathBuf::from(home).join(".copilot-shell/config.toml");
    // A missing file is fine: cosh-shell runs on defaults and creates it later.
    if !path.exists() {
        return ConfigFileStatus::consumable();
    }
    // An existing file the user intended to configure with: a read failure
    // (permissions, directory) blocks loading. For parse, mirror what the shell
    // actually consumes: load_config_file_into runs both parse_simple_config
    // (legacy `key = value`) and parse_toml_config, so a file is consumable if
    // it is valid TOML, is recognized by the legacy simple parser, or carries
    // no meaningful content.
    match std::fs::read_to_string(&path) {
        Ok(content) => ConfigFileStatus {
            readable: true,
            parseable: config_content_consumable(&content),
        },
        Err(_) => ConfigFileStatus {
            readable: false,
            parseable: false,
        },
    }
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
    "health.enabled",
    "health.role",
    "health.memory_sensitive",
    "health.verbose",
];

fn config_content_consumable(content: &str) -> bool {
    // Valid TOML loads cleanly.
    if content.parse::<toml::Value>().is_ok() {
        return true;
    }
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

fn collect_config(builder: &mut HealthReportBuilder, config: &CoshConfig, elapsed_ms: u128) {
    let home_present = std::env::var_os("HOME")
        .map(|home| !home.is_empty())
        .unwrap_or(false);
    let status = config_file_status();
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
        Some(root) => is_trusted_root(root, &config.trusted_project_roots),
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

fn is_trusted_root(root: &Path, trusted_roots: &[PathBuf]) -> bool {
    let root = canonical_root(root);
    trusted_roots
        .iter()
        .any(|trusted| canonical_root(trusted) == root)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionsReadiness {
    Ok,
    Unwritable,
}

/// Pure classification for config-directory writability. No I/O.
pub(crate) fn classify_permissions(writable: bool) -> PermissionsReadiness {
    if writable {
        PermissionsReadiness::Ok
    } else {
        PermissionsReadiness::Unwritable
    }
}

fn config_state_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".copilot-shell"))
}

fn dir_writable(path: &Path) -> bool {
    // Probe actual writability by trying to create a temporary file.  This
    // catches cases that `metadata().permissions().readonly()` misses: a
    // read-only parent (HOME) when the target does not exist, or a directory
    // with write bit but no execute/search bit.
    let probe_dir = if path.is_dir() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent() {
        if parent.is_dir() {
            parent.to_path_buf()
        } else {
            return false;
        }
    } else {
        return false;
    };
    let probe = probe_dir.join(format!(".cosh-write-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn collect_permissions(builder: &mut HealthReportBuilder, elapsed_ms: u128) {
    let dir = config_state_dir();
    let (path_str, writable) = match &dir {
        Some(path) => (path.display().to_string(), dir_writable(path)),
        None => (String::new(), false),
    };
    builder
        .add_fact(
            HealthFactCategory::Permissions,
            "permissions.state_dir",
            HealthFactValue::String(path_str.clone()),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Permissions,
            "permissions.state_dir_writable",
            HealthFactValue::Bool(writable),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_check_done("permissions");

    if classify_permissions(writable) == PermissionsReadiness::Unwritable {
        let mut args = BTreeMap::new();
        args.insert("path".to_string(), path_str);
        builder.add_finding(env_finding(
            "env-permissions",
            HealthMessageId::HealthFindingPermissionsUnwritable,
            HealthMessageId::HealthRemediationPermissions,
            args,
            vec!["permissions.state_dir_writable".to_string()],
        ));
    }
}

// ─── Runtime (run registry + crash records) ──────────────────────────────

/// Crash records within this window surface as critical findings.
const CRASH_WINDOW_MS: u128 = 24 * 60 * 60 * 1000;

/// One parsed crash log line (written by `diagnostics::crash` and cosh-core's
/// `crash` module). Shared with the doctor render layer for the header
/// summary line.
pub(crate) struct CrashRecord {
    pub(crate) kind: String,
    pub(crate) ts_ms: u128,
    pub(crate) panic: String,
}

fn env_now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

pub(crate) fn ts_label(ts_ms: u128) -> String {
    let millis = ts_ms.min(i64::MAX as u128) as i64;
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|ts| {
            ts.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| ts_ms.to_string())
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "core" => "cosh-core",
        _ => "cosh-shell",
    }
}

fn crash_file_paths() -> Vec<(String, PathBuf)> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let base = PathBuf::from(home).join(".copilot-shell");
    vec![
        ("cosh-shell".to_string(), base.join("cosh-shell-crash.log")),
        ("cosh-core".to_string(), base.join("cosh-core-crash.log")),
    ]
}

/// Parses both crash logs and keeps only the records inside `CRASH_WINDOW_MS`,
/// oldest first. Unparseable lines are skipped so a truncated tail never
/// breaks the collector.
pub(crate) fn recent_crash_records(now_ms: u128) -> Vec<CrashRecord> {
    let mut records = Vec::new();
    for (kind, path) in crash_file_paths() {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let ts_ms = value
                .get("ts")
                .and_then(serde_json::Value::as_u64)
                .map(u128::from)
                .unwrap_or_default();
            if now_ms.saturating_sub(ts_ms) > CRASH_WINDOW_MS {
                continue;
            }
            records.push(CrashRecord {
                kind: kind.clone(),
                ts_ms,
                panic: value
                    .get("panic")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
    }
    records.sort_by_key(|record| record.ts_ms);
    records
}

/// Runtime evidence: live shell/core instances from the run registry, stale
/// entries left by abnormal exits, and 24h crash records. Pure file reads
/// plus zero-signal pid probes; never mutates the registry (cleanup stays a
/// session-startup responsibility).
fn collect_runtime(builder: &mut HealthReportBuilder, elapsed_ms: u128) {
    let now_ms = env_now_ms();
    let entries = crate::diagnostics::run_registry::read_entries();
    let crashes = recent_crash_records(now_ms);

    let mut shells: Vec<&RunEntry> = Vec::new();
    let mut cores: Vec<&RunEntry> = Vec::new();
    let mut stale: Vec<&RunEntry> = Vec::new();
    for entry in &entries {
        if crate::diagnostics::run_registry::pid_alive(entry.pid) {
            match entry.kind.as_str() {
                "shell" => shells.push(entry),
                "core" => cores.push(entry),
                _ => {}
            }
        } else {
            stale.push(entry);
        }
    }
    shells.sort_by_key(|entry| entry.pid);
    cores.sort_by_key(|entry| entry.pid);
    stale.sort_by_key(|entry| entry.pid);

    let shells_label = shells
        .iter()
        .map(|entry| {
            format!(
                "{} ({})",
                entry.pid,
                entry.shell_kind.as_deref().unwrap_or("?")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let cores_label = cores
        .iter()
        .map(|entry| match entry.session_id.as_deref() {
            Some(id) if !id.is_empty() => format!("{} (session {})", entry.pid, id),
            _ => entry.pid.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");

    builder
        .add_fact(
            HealthFactCategory::Runtime,
            "runtime.shells",
            HealthFactValue::String(shells_label),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Runtime,
            "runtime.cores",
            HealthFactValue::String(cores_label),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Runtime,
            "runtime.stale_entries",
            HealthFactValue::Unsigned(stale.len() as u64),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Runtime,
            "runtime.crashes_24h",
            HealthFactValue::Unsigned(crashes.len() as u64),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_check_done("runtime");

    // Orphan cores: alive but no living shell owns them. Brokered cores are
    // owned by an agent host, not a cosh shell, so they are never orphans.
    for core in &cores {
        if core.mode.as_deref() == Some("brokered") {
            continue;
        }
        let orphaned = match core.owner_shell_pid {
            Some(owner) => !crate::diagnostics::run_registry::pid_alive(owner),
            None => true,
        };
        if orphaned {
            let mut args = BTreeMap::new();
            args.insert("pid".to_string(), core.pid.to_string());
            builder.add_finding(env_finding(
                &format!("env-runtime-orphan-{}", core.pid),
                HealthMessageId::HealthFindingOrphanCore,
                HealthMessageId::HealthRemediationOrphanCore,
                args,
                vec!["runtime.cores".to_string()],
            ));
        }
    }

    // Stale entries: the process died without cleaning up its entry.
    for entry in &stale {
        let mut args = BTreeMap::new();
        args.insert("kind".to_string(), kind_label(&entry.kind).to_string());
        args.insert("pid".to_string(), entry.pid.to_string());
        args.insert("time".to_string(), ts_label(entry.start_ts_ms));
        builder.add_finding(env_finding(
            &format!("env-runtime-stale-{}", entry.pid),
            HealthMessageId::HealthFindingStaleEntry,
            HealthMessageId::HealthRemediationStaleEntry,
            args,
            vec!["runtime.stale_entries".to_string()],
        ));
    }

    // Crash records in the last 24h are the highest-priority product fact.
    for crash in &crashes {
        let mut panic = crate::evidence::redact_sensitive_text(&crash.panic).0;
        if panic.chars().count() > 120 {
            panic = format!("{}…", panic.chars().take(120).collect::<String>());
        }
        let mut args = BTreeMap::new();
        args.insert("kind".to_string(), crash.kind.clone());
        args.insert("time".to_string(), ts_label(crash.ts_ms));
        args.insert("panic".to_string(), panic);
        builder.add_finding(HealthFinding {
            id: format!("env-runtime-crash-{}-{}", crash.kind, crash.ts_ms),
            severity: HealthSeverity::Critical,
            category: HealthFindingCategory::Anomaly,
            title_id: HealthMessageId::HealthFindingCrash,
            detail_id: Some(HealthMessageId::HealthRemediationCrash),
            detail_args: args,
            evidence_fact_ids: vec!["runtime.crashes_24h".to_string()],
            suggested_try_ids: Vec::new(),
        });
    }
}

// ─── Logs (runtime WARN/ERROR scan) ──────────────────────────────────────

/// Bounded tail read per log file; keeps the scan under 10ms no matter how
/// large the daily files grow.
pub(crate) const LOG_TAIL_MAX_BYTES: u64 = 256 * 1024;

/// WARN lines per file within 24h above this threshold surface as a finding
/// (retry-loop indicator).
const WARN_FLOOD_THRESHOLD: u64 = 100;

/// Max length of the redacted ERROR sample attached to findings.
const LOG_SAMPLE_MAX_CHARS: usize = 200;

/// Daily log file kinds written by `runtime/logging.rs` (shell) and
/// cosh-core's logging setup.
const LOG_FILE_KINDS: [&str; 2] = ["cosh-shell.log", "cosh-core.log"];

/// Per-kind scan result aggregated over the candidate files.
#[derive(Default)]
struct LogScan {
    warns: u64,
    errors: u64,
    last_error_ts_ms: Option<u128>,
    last_error_line: Option<String>,
}

/// Candidate log paths: today and yesterday for each kind. Counts only
/// consider lines inside the 24h window, so files older than yesterday can
/// never contribute.
pub(crate) fn log_file_paths() -> Vec<(String, PathBuf)> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let dir = PathBuf::from(home).join(".copilot-shell/logs");
    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let yesterday = now
        .checked_sub_days(chrono::Days::new(1))
        .map(|ts| ts.format("%Y-%m-%d").to_string());
    let mut paths = Vec::new();
    for kind in LOG_FILE_KINDS {
        paths.push((kind.to_string(), dir.join(format!("{kind}.{today}"))));
        if let Some(date) = &yesterday {
            paths.push((kind.to_string(), dir.join(format!("{kind}.{date}"))));
        }
    }
    paths
}

/// Read at most `max_bytes` from the tail of `path`. When the file is
/// truncated from the start, the first partial line is dropped so line-based
/// parsing always sees complete lines.
pub(crate) fn read_log_tail(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        return Some(String::new());
    }
    let read_from = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(read_from)).ok()?;
    let mut buf = Vec::new();
    file.take(max_bytes).read_to_end(&mut buf).ok()?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if read_from > 0 {
        if let Some(pos) = text.find('\n') {
            text.drain(..=pos);
        }
    }
    Some(text)
}

/// Parse one log header line in the tracing fmt default layout:
/// `<RFC3339 ts> <LEVEL> <target>: <message>`. Continuation lines (no
/// timestamp prefix) are skipped. Returns the level and timestamp for
/// WARN/ERROR lines inside the 24h window.
pub(crate) fn parse_log_header(line: &str, now_ms: u128) -> Option<(&str, u128)> {
    let mut tokens = line.split_whitespace();
    let ts_token = tokens.next()?;
    let level = tokens.next()?;
    if level != "WARN" && level != "ERROR" {
        return None;
    }
    let ts = chrono::DateTime::parse_from_rfc3339(ts_token).ok()?;
    let ts_ms = u128::try_from(ts.timestamp_millis()).ok()?;
    if now_ms.saturating_sub(ts_ms) > CRASH_WINDOW_MS {
        return None;
    }
    Some((level, ts_ms))
}

/// Runtime error evidence from the daily logs: WARN/ERROR counts in the 24h
/// window, freshness of the last write, and one redacted ERROR sample line.
/// Findings surface per file (errors present, WARN flood) with the shared
/// remediation pointing at the diagnostics export.
fn collect_logs(builder: &mut HealthReportBuilder, elapsed_ms: u128) {
    let now_ms = env_now_ms();
    let mut per_kind: BTreeMap<String, LogScan> = BTreeMap::new();
    let mut any_file = false;
    let mut last_write_ms = 0u128;

    for (kind, path) in log_file_paths() {
        let Some(text) = read_log_tail(&path, LOG_TAIL_MAX_BYTES) else {
            continue;
        };
        any_file = true;
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(modified) = meta.modified() {
                if let Ok(since) = modified.duration_since(UNIX_EPOCH) {
                    last_write_ms = last_write_ms.max(since.as_millis());
                }
            }
        }
        let scan = per_kind.entry(kind.clone()).or_default();
        for line in text.lines() {
            let Some((level, ts_ms)) = parse_log_header(line, now_ms) else {
                continue;
            };
            if level == "WARN" {
                scan.warns += 1;
                continue;
            }
            scan.errors += 1;
            if scan.last_error_ts_ms.is_none_or(|last| ts_ms >= last) {
                scan.last_error_ts_ms = Some(ts_ms);
                scan.last_error_line = Some(line.to_string());
            }
        }
    }

    let total_warns: u64 = per_kind.values().map(|scan| scan.warns).sum();
    let total_errors: u64 = per_kind.values().map(|scan| scan.errors).sum();
    let latest_error = per_kind
        .values()
        .filter_map(|scan| {
            scan.last_error_ts_ms
                .map(|ts_ms| (ts_ms, scan.last_error_line.clone().unwrap_or_default()))
        })
        .max_by_key(|(ts_ms, _)| *ts_ms);
    let error_sample = match &latest_error {
        Some((_, line)) => {
            let mut sample = crate::evidence::redact_sensitive_text(line).0;
            if sample.chars().count() > LOG_SAMPLE_MAX_CHARS {
                sample = format!(
                    "{}…",
                    sample
                        .chars()
                        .take(LOG_SAMPLE_MAX_CHARS)
                        .collect::<String>()
                );
            }
            sample
        }
        None => String::new(),
    };
    let stale_24h =
        any_file && last_write_ms > 0 && now_ms.saturating_sub(last_write_ms) > CRASH_WINDOW_MS;

    builder
        .add_fact(
            HealthFactCategory::Logs,
            "logs.present",
            HealthFactValue::Bool(any_file),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Logs,
            "logs.warns_24h",
            HealthFactValue::Unsigned(total_warns),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Logs,
            "logs.errors_24h",
            HealthFactValue::Unsigned(total_errors),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Logs,
            "logs.last_write_ms",
            HealthFactValue::Unsigned(last_write_ms.min(u64::MAX as u128) as u64),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Logs,
            "logs.last_error_ms",
            HealthFactValue::Unsigned(
                latest_error
                    .map(|(ts_ms, _)| ts_ms.min(u64::MAX as u128) as u64)
                    .unwrap_or(0),
            ),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Logs,
            "logs.stale_24h",
            HealthFactValue::Bool(stale_24h),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Logs,
            "logs.error_sample",
            HealthFactValue::String(error_sample.clone()),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_check_done("logs");

    for (kind, scan) in &per_kind {
        let stem = kind.trim_end_matches(".log");
        if scan.errors > 0 {
            let mut args = BTreeMap::new();
            args.insert("count".to_string(), scan.errors.to_string());
            args.insert("file".to_string(), kind.clone());
            args.insert(
                "time".to_string(),
                scan.last_error_ts_ms.map(ts_label).unwrap_or_default(),
            );
            let mut evidence = vec!["logs.errors_24h".to_string()];
            if !error_sample.is_empty() {
                evidence.push("logs.error_sample".to_string());
            }
            builder.add_finding(env_finding(
                &format!("env-logs-errors-{stem}"),
                HealthMessageId::HealthFindingRecentErrors,
                HealthMessageId::HealthRemediationLogs,
                args,
                evidence,
            ));
        }
        if scan.warns > WARN_FLOOD_THRESHOLD {
            let mut args = BTreeMap::new();
            args.insert("count".to_string(), scan.warns.to_string());
            args.insert("file".to_string(), kind.clone());
            builder.add_finding(env_finding(
                &format!("env-logs-warn-flood-{stem}"),
                HealthMessageId::HealthFindingWarnFlood,
                HealthMessageId::HealthRemediationLogs,
                args,
                vec!["logs.warns_24h".to_string()],
            ));
        }
    }
}

#[cfg(test)]
#[path = "env_collector_tests.rs"]
mod env_collector_tests;
