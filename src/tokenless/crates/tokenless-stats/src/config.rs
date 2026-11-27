//! Configuration for tokenless.
//!
//! Stored at `~/.tokenless/config.json`. Controls global feature flags,
//! optional statistics retention windows, per-domain PreTool RTK rollout
//! switches, and dual-end command exclusions.
//! Environment variables `TOKENLESS_STATS_ENABLED`, `TOKENLESS_SLS_ENABLED`,
//! `TOKENLESS_COMPRESSION_ENABLED`, `TOKENLESS_STATS_RETENTION_DAYS`,
//! `TOKENLESS_STATS_TEXT_RETENTION_DAYS`, `TOKENLESS_RTK_ROLLOUT`, and
//! `TOKENLESS_EXCLUDE_COMMANDS` override file config at runtime.
//! `tokenless stats enable` / `disable` persist from the file snapshot so
//! those session overrides are not written back.

use crate::recorder::RetentionPolicy;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::PathBuf;

/// Built-in default automatic retention window in days for `stats.db` rows.
pub const DEFAULT_STATS_RETENTION_DAYS: u32 = RetentionPolicy::DEFAULT_ROW_RETENTION_DAYS;
/// Built-in default automatic retention window in days for stored text payloads in `stats.db`.
pub const DEFAULT_STATS_TEXT_RETENTION_DAYS: u32 = RetentionPolicy::DEFAULT_TEXT_RETENTION_DAYS;

fn default_stats_retention_days() -> Option<u32> {
    Some(DEFAULT_STATS_RETENTION_DAYS)
}

fn default_stats_text_retention_days() -> Option<u32> {
    Some(DEFAULT_STATS_TEXT_RETENTION_DAYS)
}

thread_local! {
    /// Test-only redirect for [`TokenlessConfig::config_path`].
    ///
    /// This is a thread-local API call, not an environment variable, so it
    /// does not weaken the passwd-rooted `$HOME` refusal below.
    static CONFIG_PATH_OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Per-domain rollout mode controlling PreTool RTK vs. PostTool native execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainRolloutMode {
    /// Rewrite eligible commands with embedded RTK during PreTool.
    PretoolRtk,
    /// Keep commands native in PreTool and compress via PostTool when supported.
    PosttoolNative,
}

impl DomainRolloutMode {
    /// Returns the canonical snake_case wire and config representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PretoolRtk => "pretool_rtk",
            Self::PosttoolNative => "posttool_native",
        }
    }
}

/// Domain-level routing switches stored in `~/.tokenless/config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtkRolloutConfig {
    /// Search commands (`grep`, `rg`, `find`). Defaults to `posttool_native`.
    #[serde(default = "default_posttool_native")]
    pub search: DomainRolloutMode,
    /// Version-control and diff commands (`git`, `diff`, `gt`, `yadm`). Defaults to `pretool_rtk`.
    #[serde(default = "default_pretool_rtk")]
    pub vcs: DomainRolloutMode,
    /// Build and test runners (`cargo`, `pytest`, `jest`, `go`, `make`, etc.). Defaults to `posttool_native`.
    #[serde(default = "default_posttool_native")]
    pub build_test: DomainRolloutMode,
    /// Linters and formatters (`eslint`, `ruff`, `tsc`, `golangci-lint`, etc.). Defaults to `pretool_rtk`.
    #[serde(default = "default_pretool_rtk")]
    pub lint: DomainRolloutMode,
    /// Cloud, code-hosting, container, and network CLIs (`gh`, `glab`, `docker`, `kubectl`, `aws`, `curl`, etc.). Defaults to `pretool_rtk`.
    #[serde(default = "default_pretool_rtk")]
    pub cloud_net: DomainRolloutMode,
}

fn default_posttool_native() -> DomainRolloutMode {
    DomainRolloutMode::PosttoolNative
}

fn default_pretool_rtk() -> DomainRolloutMode {
    DomainRolloutMode::PretoolRtk
}

impl Default for RtkRolloutConfig {
    fn default() -> Self {
        Self {
            search: DomainRolloutMode::PosttoolNative,
            vcs: DomainRolloutMode::PretoolRtk,
            build_test: DomainRolloutMode::PosttoolNative,
            lint: DomainRolloutMode::PretoolRtk,
            cloud_net: DomainRolloutMode::PretoolRtk,
        }
    }
}

impl RtkRolloutConfig {
    fn field_mut(&mut self, domain: &str) -> Option<&mut DomainRolloutMode> {
        match domain {
            "search" => Some(&mut self.search),
            "vcs" => Some(&mut self.vcs),
            "build_test" => Some(&mut self.build_test),
            "lint" => Some(&mut self.lint),
            "cloud_net" => Some(&mut self.cloud_net),
            _ => None,
        }
    }
}

fn apply_rollout_value(rollout: &mut RtkRolloutConfig, value: &serde_json::Value, source: &str) {
    let Some(map) = value.as_object() else {
        crate::warn_stats(&format!(
            "[tokenless] WARNING: ignoring non-object {source} value {value}"
        ));
        return;
    };
    for (domain, mode_value) in map {
        let Some(target) = rollout.field_mut(domain) else {
            crate::warn_stats(&format!(
                "[tokenless] WARNING: ignoring unknown {source} domain {domain:?}"
            ));
            continue;
        };
        match serde_json::from_value::<DomainRolloutMode>(mode_value.clone()) {
            Ok(mode) => *target = mode,
            Err(_) => {
                crate::warn_stats(&format!(
                    "[tokenless] WARNING: ignoring invalid {source}.{domain} value {mode_value} (expected \"pretool_rtk\" or \"posttool_native\")"
                ));
            }
        }
    }
}

fn parse_complete_rollout_env(raw: &str) -> Option<RtkRolloutConfig> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let map = value.as_object()?;
    if map.len() != 5 {
        return None;
    }
    Some(RtkRolloutConfig {
        search: serde_json::from_value(map.get("search")?.clone()).ok()?,
        vcs: serde_json::from_value(map.get("vcs")?.clone()).ok()?,
        build_test: serde_json::from_value(map.get("build_test")?.clone()).ok()?,
        lint: serde_json::from_value(map.get("lint")?.clone()).ok()?,
        cloud_net: serde_json::from_value(map.get("cloud_net")?.clone()).ok()?,
    })
}

fn parse_config_json(raw: &str) -> TokenlessConfig {
    let value = match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => value,
        Err(error) => {
            crate::warn_stats(&format!(
                "[tokenless] WARNING: ignoring invalid config.json: {error}"
            ));
            return TokenlessConfig::default();
        }
    };
    let Some(map) = value.as_object() else {
        crate::warn_stats("[tokenless] WARNING: ignoring non-object config.json");
        return TokenlessConfig::default();
    };
    let mut config = TokenlessConfig::default();
    if let Some(value) = map.get("stats_enabled") {
        if let Some(enabled) = value.as_bool() {
            config.stats_enabled = enabled;
        } else {
            crate::warn_stats(&format!(
                "[tokenless] WARNING: ignoring invalid config.json stats_enabled value {value}"
            ));
        }
    }
    if let Some(value) = map.get("sls_enabled") {
        if let Some(enabled) = value.as_bool() {
            config.sls_enabled = enabled;
        } else {
            crate::warn_stats(&format!(
                "[tokenless] WARNING: ignoring invalid config.json sls_enabled value {value}"
            ));
        }
    }
    if let Some(value) = map.get("compression_enabled") {
        if let Some(enabled) = value.as_bool() {
            config.compression_enabled = enabled;
        } else {
            crate::warn_stats(&format!(
                "[tokenless] WARNING: ignoring invalid config.json compression_enabled value {value}"
            ));
        }
    }
    if let Some(value) = map.get("stats_retention_days") {
        match serde_json::from_value::<Option<u32>>(value.clone()) {
            Ok(days) => config.stats_retention_days = days,
            Err(_) => {
                crate::warn_stats(&format!(
                    "[tokenless] WARNING: ignoring invalid config.json stats_retention_days value {value} (expected non-negative integer or null)"
                ));
            }
        }
    }
    if let Some(value) = map.get("stats_text_retention_days") {
        match serde_json::from_value::<Option<u32>>(value.clone()) {
            Ok(days) => config.stats_text_retention_days = days,
            Err(_) => {
                crate::warn_stats(&format!(
                    "[tokenless] WARNING: ignoring invalid config.json stats_text_retention_days value {value} (expected non-negative integer or null)"
                ));
            }
        }
    }
    if let Some(value) = map.get("rtk_rollout") {
        apply_rollout_value(&mut config.rtk_rollout, value, "config.json rtk_rollout");
    }
    if let Some(value) = map.get("exclude_commands") {
        match serde_json::from_value::<Vec<String>>(value.clone()) {
            Ok(commands) => config.exclude_commands = commands,
            Err(_) => {
                crate::warn_stats(&format!(
                    "[tokenless] WARNING: ignoring invalid config.json exclude_commands value {value} (expected JSON array of strings)"
                ));
            }
        }
    }
    config
}

/// Global tokenless configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenlessConfig {
    /// Whether to record compression stats (default: true)
    #[serde(default = "default_true")]
    pub stats_enabled: bool,
    /// Whether SLS integration is enabled (default: true). When enabled,
    /// each compression is also appended as a JSONL record for SLS ingestion.
    #[serde(default = "default_true")]
    pub sls_enabled: bool,
    /// Whether compression is actually applied (default: true).
    /// When false, tokenless runs in dry-run mode: it computes and records
    /// the predicted savings but emits the original (uncompressed) text,
    /// enabling A/B comparison of the same task with/without compression.
    #[serde(default = "default_true")]
    pub compression_enabled: bool,
    /// Optional automatic retention window in days for `stats.db` rows
    /// (plus orphaned `compression_artifacts` and old `retrieve_events`).
    /// `None` or `0` disables automatic row expiry (default: `Some(30)`).
    #[serde(default = "default_stats_retention_days")]
    pub stats_retention_days: Option<u32>,
    /// Optional automatic retention window in days for full text payloads
    /// (`before_text`, `after_text`, `before_output`, `after_output`) in
    /// `stats.db`. Older rows retain numeric counts and metadata with `NULL`
    /// text. `None` or `0` disables automatic text trimming (default: `Some(7)`).
    #[serde(default = "default_stats_text_retention_days")]
    pub stats_text_retention_days: Option<u32>,
    /// Domain-level switches between PreTool RTK and PostTool native compression.
    #[serde(default)]
    pub rtk_rollout: RtkRolloutConfig,
    /// Command names or word prefixes excluded from both PreTool RTK rewriting
    /// and PostTool output compression.
    #[serde(default)]
    pub exclude_commands: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for TokenlessConfig {
    fn default() -> Self {
        Self {
            stats_enabled: true,
            sls_enabled: true,
            compression_enabled: true,
            stats_retention_days: default_stats_retention_days(),
            stats_text_retention_days: default_stats_text_retention_days(),
            rtk_rollout: RtkRolloutConfig::default(),
            exclude_commands: Vec::new(),
        }
    }
}

/// Parse an optional retention-days environment variable override.
///
/// - `None`, empty/whitespace-only, or unparseable values return `None` (no
///   override; fall back to `config.json` or default).
/// - `"0"` returns `Some(None)` (explicitly disables retention, overriding a
///   non-zero value in `config.json`).
/// - A positive integer returns `Some(Some(days))`.
pub fn parse_retention_env_override(raw: Option<&str>) -> Option<Option<u32>> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<u32>() {
        Ok(0) => Some(None),
        Ok(days) => Some(Some(days)),
        Err(_) => None,
    }
}

#[derive(Default, Clone, Copy)]
struct ConfigEnvOverrides<'a> {
    stats_enabled: Option<&'a str>,
    sls_enabled: Option<&'a str>,
    compression_enabled: Option<&'a str>,
    stats_retention_days: Option<&'a str>,
    stats_text_retention_days: Option<&'a str>,
    rtk_rollout: Option<&'a str>,
    exclude_commands: Option<&'a str>,
}

/// Parse a boolean env value: "1", "true", "yes" (case-insensitive) → true.
/// All other non-empty values — including "0", "false", "no" — return false.
/// (Empty strings are filtered to `None` by callers before reaching this
/// function, so they never reach here.)
fn parse_env_bool(val: &str) -> bool {
    val == "1" || val.eq_ignore_ascii_case("true") || val.eq_ignore_ascii_case("yes")
}

impl TokenlessConfig {
    fn config_path() -> PathBuf {
        if let Some(path) = CONFIG_PATH_OVERRIDE.with(|slot| slot.borrow().clone()) {
            return path;
        }
        // Resolve home via the shared passwd-rooted helper so an attacker
        // cannot redirect the config path by setting $HOME before invoking
        // any tokenless binary. When no trusted home is available, return
        // a path under /dev/null so the open/create call fails loudly
        // (ENOENT or ENOTDIR) rather than silently landing in the CWD
        // (which PathBuf::from("").join(...) would produce).
        let home = crate::home::get_home_dir();
        if home.is_empty() {
            return PathBuf::from("/dev/null/.tokenless/config.json");
        }
        PathBuf::from(home).join(".tokenless/config.json")
    }

    /// Redirect [`Self::config_path`] for the current thread.
    ///
    /// Production CLI never calls this. Tests use it so [`Self::load`],
    /// [`Self::load_from_file`], and [`Self::save`] can run against an
    /// isolated file without writing the passwd-backed
    /// `~/.tokenless/config.json`.
    #[doc(hidden)]
    pub fn override_config_path_for_tests(path: Option<PathBuf>) {
        CONFIG_PATH_OVERRIDE.with(|slot| *slot.borrow_mut() = path);
    }

    /// Whether a config file exists on disk.
    pub fn config_file_exists() -> bool {
        Self::config_path().exists()
    }

    /// Inspect whether `stats_retention_days` and `stats_text_retention_days`
    /// keys are explicitly present with valid values in the on-disk config file.
    ///
    /// Returns `(false, false)` when the config file cannot be read or is not a
    /// valid JSON object; each individual retention key is checked independently
    /// to match [`Self::load`]'s field-by-field parsing.
    pub fn file_retention_keys_present() -> (bool, bool) {
        let Ok(raw) = std::fs::read_to_string(Self::config_path()) else {
            return (false, false);
        };
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&raw)
        else {
            return (false, false);
        };
        let has_valid_key = |key: &str| {
            map.get(key)
                .is_some_and(|v| serde_json::from_value::<Option<u32>>(v.clone()).is_ok())
        };
        (
            has_valid_key("stats_retention_days"),
            has_valid_key("stats_text_retention_days"),
        )
    }

    /// Load config with explicit env overrides for all toggles and optional custom path.
    ///
    /// Priority (per setting): env > config.json file > default.
    /// Empty env var values are normalized to None (treated as unset).
    ///
    /// All env combinations that leave at least one setting unset will attempt to
    /// read the config file so the file value can fill the gap. IO failures
    /// (missing file, unreadable path, corrupt NFS mount, etc.) are silently
    /// ignored and the built-in defaults take over — `env > default` priority is
    /// preserved even when the file cannot be read.
    ///
    /// Fast path: when all five base settings are determined by env vars, the
    /// file read is skipped entirely — every setting used by [`Self::load`] is
    /// already determined, so opening the config file would have no effect.
    pub fn load_with_envs_and_path(
        stats_env: Option<&str>,
        sls_env: Option<&str>,
        compression_env: Option<&str>,
        path: Option<&PathBuf>,
    ) -> Self {
        Self::load_with_all_envs_and_file_reader(
            ConfigEnvOverrides {
                stats_enabled: stats_env,
                sls_enabled: sls_env,
                compression_enabled: compression_env,
                ..ConfigEnvOverrides::default()
            },
            path,
            |p| std::fs::read_to_string(p),
        )
    }

    /// Core logic of [`TokenlessConfig::load_with_envs_and_path`] with an
    /// injectable file reader. Production always reads via
    /// [`std::fs::read_to_string`]; tests inject a recording reader so they can
    /// assert whether the config file read was attempted at all.
    #[cfg(test)]
    fn load_with_envs_and_file_reader(
        stats_env: Option<&str>,
        sls_env: Option<&str>,
        compression_env: Option<&str>,
        path: Option<&PathBuf>,
        read_file: impl FnOnce(&std::path::Path) -> std::io::Result<String>,
    ) -> Self {
        Self::load_with_all_envs_and_file_reader(
            ConfigEnvOverrides {
                stats_enabled: stats_env,
                sls_enabled: sls_env,
                compression_enabled: compression_env,
                ..ConfigEnvOverrides::default()
            },
            path,
            read_file,
        )
    }

    fn load_with_all_envs_and_file_reader(
        envs: ConfigEnvOverrides<'_>,
        path: Option<&PathBuf>,
        read_file: impl FnOnce(&std::path::Path) -> std::io::Result<String>,
    ) -> Self {
        // Normalize empty strings to None — an empty env var means "unset".
        let stats_env = envs.stats_enabled.filter(|v| !v.is_empty());
        let sls_env = envs.sls_enabled.filter(|v| !v.is_empty());
        let compression_env = envs.compression_enabled.filter(|v| !v.is_empty());
        let retention_env = parse_retention_env_override(envs.stats_retention_days);
        let text_retention_env = parse_retention_env_override(envs.stats_text_retention_days);

        // Fast path: all five base settings are determined by env — no file read needed.
        if let (Some(s), Some(sl), Some(c), Some(r), Some(tr)) = (
            stats_env,
            sls_env,
            compression_env,
            retention_env,
            text_retention_env,
        ) {
            return Self {
                stats_enabled: parse_env_bool(s),
                sls_enabled: parse_env_bool(sl),
                compression_enabled: parse_env_bool(c),
                stats_retention_days: r,
                stats_text_retention_days: tr,
                rtk_rollout: RtkRolloutConfig::default(),
                exclude_commands: Vec::new(),
            };
        }

        let default_path = Self::config_path();
        let config_path = path.unwrap_or(&default_path);
        let base = read_file(config_path)
            .ok()
            .map(|s| parse_config_json(&s))
            .unwrap_or_default();

        base.apply_env_overrides(ConfigEnvOverrides {
            stats_enabled: stats_env,
            sls_enabled: sls_env,
            compression_enabled: compression_env,
            stats_retention_days: envs.stats_retention_days,
            stats_text_retention_days: envs.stats_text_retention_days,
            rtk_rollout: None,
            exclude_commands: None,
        })
    }

    fn load_with_policy_and_file_reader(
        envs: ConfigEnvOverrides<'_>,
        path: Option<&PathBuf>,
        read_file: impl FnOnce(&std::path::Path) -> std::io::Result<String>,
    ) -> Self {
        let stats_env = envs.stats_enabled.filter(|v| !v.is_empty());
        let sls_env = envs.sls_enabled.filter(|v| !v.is_empty());
        let compression_env = envs.compression_enabled.filter(|v| !v.is_empty());
        let retention_env = parse_retention_env_override(envs.stats_retention_days);
        let text_retention_env = parse_retention_env_override(envs.stats_text_retention_days);
        let rollout_env = envs.rtk_rollout.filter(|v| !v.is_empty());
        let exclude_env = envs.exclude_commands.filter(|v| !v.is_empty());

        // Fast path: when all five base settings, a complete five-domain
        // rollout object, and a valid exclusion list are given via env, no
        // on-disk field can affect the result — skip opening config.json so a
        // stalled home mount cannot exhaust PreTool deadlines.
        if let (
            Some(s),
            Some(sl),
            Some(c),
            Some(r),
            Some(tr),
            Some(rtk_rollout),
            Some(exclude_commands),
        ) = (
            stats_env,
            sls_env,
            compression_env,
            retention_env,
            text_retention_env,
            rollout_env.and_then(parse_complete_rollout_env),
            exclude_env.and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok()),
        ) {
            return Self {
                stats_enabled: parse_env_bool(s),
                sls_enabled: parse_env_bool(sl),
                compression_enabled: parse_env_bool(c),
                stats_retention_days: r,
                stats_text_retention_days: tr,
                rtk_rollout,
                exclude_commands,
            };
        }

        let default_path = Self::config_path();
        let config_path = path.unwrap_or(&default_path);
        let base = read_file(config_path)
            .ok()
            .map(|s| parse_config_json(&s))
            .unwrap_or_default();

        base.apply_env_overrides(ConfigEnvOverrides {
            stats_enabled: stats_env,
            sls_enabled: sls_env,
            compression_enabled: compression_env,
            stats_retention_days: envs.stats_retention_days,
            stats_text_retention_days: envs.stats_text_retention_days,
            rtk_rollout: rollout_env,
            exclude_commands: exclude_env,
        })
    }

    fn apply_env_overrides(mut self, envs: ConfigEnvOverrides<'_>) -> Self {
        if let Some(val) = envs.stats_enabled.filter(|v| !v.is_empty()) {
            self.stats_enabled = parse_env_bool(val);
        }
        if let Some(val) = envs.sls_enabled.filter(|v| !v.is_empty()) {
            self.sls_enabled = parse_env_bool(val);
        }
        if let Some(val) = envs.compression_enabled.filter(|v| !v.is_empty()) {
            self.compression_enabled = parse_env_bool(val);
        }
        self.stats_retention_days = parse_retention_env_override(envs.stats_retention_days)
            .unwrap_or_else(|| self.stats_retention_days());
        self.stats_text_retention_days =
            parse_retention_env_override(envs.stats_text_retention_days)
                .unwrap_or_else(|| self.stats_text_retention_days());
        if let Some(val) = envs.rtk_rollout.filter(|v| !v.is_empty()) {
            match serde_json::from_str::<serde_json::Value>(val) {
                Ok(parsed) => {
                    apply_rollout_value(&mut self.rtk_rollout, &parsed, "TOKENLESS_RTK_ROLLOUT");
                }
                Err(error) => {
                    crate::warn_stats(&format!(
                        "[tokenless] WARNING: ignoring invalid TOKENLESS_RTK_ROLLOUT JSON: {error}"
                    ));
                }
            }
        }
        if let Some(val) = envs.exclude_commands.filter(|v| !v.is_empty()) {
            match serde_json::from_str::<Vec<String>>(val) {
                Ok(commands) => self.exclude_commands = commands,
                Err(error) => {
                    crate::warn_stats(&format!(
                        "[tokenless] WARNING: ignoring invalid TOKENLESS_EXCLUDE_COMMANDS JSON: {error}"
                    ));
                }
            }
        }
        self
    }

    /// Apply process environment overrides onto this configuration.
    ///
    /// Precedence is `env > file > default` for the boolean switches, retention
    /// day windows, `TOKENLESS_RTK_ROLLOUT` (JSON object patching individual
    /// domains), and `TOKENLESS_EXCLUDE_COMMANDS` (JSON string array replacing
    /// the exclusion list). Malformed overrides emit a soft warning and leave
    /// the underlying values intact.
    #[must_use]
    pub fn with_env_overrides(self) -> Self {
        let stats_env = std::env::var("TOKENLESS_STATS_ENABLED").ok();
        let sls_env = std::env::var("TOKENLESS_SLS_ENABLED").ok();
        let compression_env = std::env::var("TOKENLESS_COMPRESSION_ENABLED").ok();
        let retention_env = std::env::var("TOKENLESS_STATS_RETENTION_DAYS").ok();
        let text_retention_env = std::env::var("TOKENLESS_STATS_TEXT_RETENTION_DAYS").ok();
        let rollout_env = std::env::var("TOKENLESS_RTK_ROLLOUT").ok();
        let exclude_env = std::env::var("TOKENLESS_EXCLUDE_COMMANDS").ok();
        self.apply_env_overrides(ConfigEnvOverrides {
            stats_enabled: stats_env.as_deref(),
            sls_enabled: sls_env.as_deref(),
            compression_enabled: compression_env.as_deref(),
            stats_retention_days: retention_env.as_deref(),
            stats_text_retention_days: text_retention_env.as_deref(),
            rtk_rollout: rollout_env.as_deref(),
            exclude_commands: exclude_env.as_deref(),
        })
    }

    /// Load `config.json` and overlay all process environment overrides.
    ///
    /// Unlike [`Self::load`], this still reads `config.json` when only the
    /// base environment variables are exported, so file-backed `rtk_rollout`
    /// and `exclude_commands` stay effective. When all five base variables, a
    /// complete five-domain `TOKENLESS_RTK_ROLLOUT`, and a valid
    /// `TOKENLESS_EXCLUDE_COMMANDS` are supplied via environment, the file
    /// read is skipped.
    #[must_use]
    pub fn load_with_policy() -> Self {
        let stats_env = std::env::var("TOKENLESS_STATS_ENABLED").ok();
        let sls_env = std::env::var("TOKENLESS_SLS_ENABLED").ok();
        let compression_env = std::env::var("TOKENLESS_COMPRESSION_ENABLED").ok();
        let retention_env = std::env::var("TOKENLESS_STATS_RETENTION_DAYS").ok();
        let text_retention_env = std::env::var("TOKENLESS_STATS_TEXT_RETENTION_DAYS").ok();
        let rollout_env = std::env::var("TOKENLESS_RTK_ROLLOUT").ok();
        let exclude_env = std::env::var("TOKENLESS_EXCLUDE_COMMANDS").ok();
        Self::load_with_policy_and_file_reader(
            ConfigEnvOverrides {
                stats_enabled: stats_env.as_deref(),
                sls_enabled: sls_env.as_deref(),
                compression_enabled: compression_env.as_deref(),
                stats_retention_days: retention_env.as_deref(),
                stats_text_retention_days: text_retention_env.as_deref(),
                rtk_rollout: rollout_env.as_deref(),
                exclude_commands: exclude_env.as_deref(),
            },
            None,
            |p| std::fs::read_to_string(p),
        )
    }

    fn load_for_post_tool_and_file_reader(
        envs: ConfigEnvOverrides<'_>,
        path: Option<&PathBuf>,
        read_file: impl FnOnce(&std::path::Path) -> std::io::Result<String>,
    ) -> Self {
        let stats_env = envs.stats_enabled.filter(|v| !v.is_empty());
        let sls_env = envs.sls_enabled.filter(|v| !v.is_empty());
        let compression_env = envs.compression_enabled.filter(|v| !v.is_empty());
        let retention_env = parse_retention_env_override(envs.stats_retention_days);
        let text_retention_env = parse_retention_env_override(envs.stats_text_retention_days);
        let exclude_env = envs.exclude_commands.filter(|v| !v.is_empty());

        // Fast path: PostTool does not read `rtk_rollout`, so all five base
        // settings plus a valid `TOKENLESS_EXCLUDE_COMMANDS` settle every
        // PostTool setting without touching `config.json`.
        if let (Some(s), Some(sl), Some(c), Some(r), Some(tr), Some(exclude_commands)) = (
            stats_env,
            sls_env,
            compression_env,
            retention_env,
            text_retention_env,
            exclude_env.and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok()),
        ) {
            return Self {
                stats_enabled: parse_env_bool(s),
                sls_enabled: parse_env_bool(sl),
                compression_enabled: parse_env_bool(c),
                stats_retention_days: r,
                stats_text_retention_days: tr,
                rtk_rollout: RtkRolloutConfig::default(),
                exclude_commands,
            };
        }

        let default_path = Self::config_path();
        let config_path = path.unwrap_or(&default_path);
        let base = read_file(config_path)
            .ok()
            .map(|s| parse_config_json(&s))
            .unwrap_or_default();

        base.apply_env_overrides(ConfigEnvOverrides {
            stats_enabled: stats_env,
            sls_enabled: sls_env,
            compression_enabled: compression_env,
            stats_retention_days: envs.stats_retention_days,
            stats_text_retention_days: envs.stats_text_retention_days,
            rtk_rollout: None,
            exclude_commands: exclude_env,
        })
    }

    /// Load `config.json` and overlay environment overrides needed by PostTool.
    ///
    /// Skips reading `config.json` when all five base environment variables
    /// and a valid `TOKENLESS_EXCLUDE_COMMANDS` are set, without requiring
    /// `TOKENLESS_RTK_ROLLOUT`.
    #[must_use]
    pub fn load_for_post_tool() -> Self {
        let stats_env = std::env::var("TOKENLESS_STATS_ENABLED").ok();
        let sls_env = std::env::var("TOKENLESS_SLS_ENABLED").ok();
        let compression_env = std::env::var("TOKENLESS_COMPRESSION_ENABLED").ok();
        let retention_env = std::env::var("TOKENLESS_STATS_RETENTION_DAYS").ok();
        let text_retention_env = std::env::var("TOKENLESS_STATS_TEXT_RETENTION_DAYS").ok();
        let exclude_env = std::env::var("TOKENLESS_EXCLUDE_COMMANDS").ok();
        Self::load_for_post_tool_and_file_reader(
            ConfigEnvOverrides {
                stats_enabled: stats_env.as_deref(),
                sls_enabled: sls_env.as_deref(),
                compression_enabled: compression_env.as_deref(),
                stats_retention_days: retention_env.as_deref(),
                stats_text_retention_days: text_retention_env.as_deref(),
                rtk_rollout: None,
                exclude_commands: exclude_env.as_deref(),
            },
            None,
            |p| std::fs::read_to_string(p),
        )
    }

    /// Load config with explicit env overrides for stats and sls toggles.
    pub fn load_with_envs(stats_env: Option<&str>, sls_env: Option<&str>) -> Self {
        Self::load_with_envs_and_path(stats_env, sls_env, None, None)
    }

    /// Load config with an explicit env override value and optional custom path.
    /// Backward-compatible wrapper: only overrides stats_enabled.
    pub fn load_with_env_and_path(env_val: Option<&str>, path: Option<&PathBuf>) -> Self {
        Self::load_with_envs_and_path(env_val, None, None, path)
    }

    /// Load config with an explicit env override value.
    /// Backward-compatible wrapper: only overrides stats_enabled.
    pub fn load_with_env(env_val: Option<&str>) -> Self {
        Self::load_with_envs(env_val, None)
    }

    /// Load config: env vars override file config, file config overrides defaults.
    /// Priority: env > config.json file > default (per setting)
    /// Empty env var values are treated as unset (fall through to file config).
    pub fn load() -> Self {
        let stats_env = std::env::var("TOKENLESS_STATS_ENABLED").ok();
        let sls_env = std::env::var("TOKENLESS_SLS_ENABLED").ok();
        let compression_env = std::env::var("TOKENLESS_COMPRESSION_ENABLED").ok();
        let retention_env = std::env::var("TOKENLESS_STATS_RETENTION_DAYS").ok();
        let text_retention_env = std::env::var("TOKENLESS_STATS_TEXT_RETENTION_DAYS").ok();
        Self::load_with_all_envs_and_file_reader(
            ConfigEnvOverrides {
                stats_enabled: stats_env.as_deref(),
                sls_enabled: sls_env.as_deref(),
                compression_enabled: compression_env.as_deref(),
                stats_retention_days: retention_env.as_deref(),
                stats_text_retention_days: text_retention_env.as_deref(),
                ..ConfigEnvOverrides::default()
            },
            None,
            |p| std::fs::read_to_string(p),
        )
    }

    /// Load on-disk config without applying process environment overrides.
    ///
    /// `tokenless stats enable` / `disable` persist only the stats toggle.
    /// Loading via [`Self::load`] would copy session env overrides such as
    /// `TOKENLESS_COMPRESSION_ENABLED` into `config.json`, turning a
    /// temporary A/B dry-run into a durable setting.
    pub fn load_from_file() -> Self {
        Self::load_with_all_envs_and_file_reader(ConfigEnvOverrides::default(), None, |p| {
            std::fs::read_to_string(p)
        })
    }

    /// Save config to disk.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        // Restrict to owner-only — the config may contain per-user
        // settings that should not be readable by other local users.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
        }
        Ok(())
    }

    /// Returns true if stats recording is enabled (env override or file config).
    pub fn is_stats_enabled(&self) -> bool {
        self.stats_enabled
    }

    /// Returns true if SLS integration is enabled (env override or file config).
    pub fn is_sls_enabled(&self) -> bool {
        self.sls_enabled
    }

    /// Returns true if compression is applied (env override or file config).
    /// When false, tokenless runs in dry-run mode.
    pub fn is_compression_enabled(&self) -> bool {
        self.compression_enabled
    }

    /// Configured automatic stats row retention window in days (`None` when disabled).
    pub fn stats_retention_days(&self) -> Option<u32> {
        self.stats_retention_days.filter(|&days| days > 0)
    }

    /// Configured automatic text-payload retention window in days (`None` when disabled).
    pub fn stats_text_retention_days(&self) -> Option<u32> {
        self.stats_text_retention_days.filter(|&days| days > 0)
    }

    /// Returns the effective [`RetentionPolicy`] for `stats.db`.
    pub fn retention_policy(&self) -> RetentionPolicy {
        RetentionPolicy {
            row_retention_days: self.stats_retention_days(),
            text_retention_days: self.stats_text_retention_days(),
        }
    }
}

#[cfg(test)]
mod tests;
