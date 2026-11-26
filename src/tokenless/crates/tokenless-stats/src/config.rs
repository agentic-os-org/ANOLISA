//! Configuration for tokenless.
//!
//! Stored at `~/.tokenless/config.json`. Controls global feature flags and
//! optional statistics retention windows.
//! Environment variables `TOKENLESS_STATS_ENABLED`, `TOKENLESS_SLS_ENABLED`,
//! `TOKENLESS_COMPRESSION_ENABLED`, `TOKENLESS_STATS_RETENTION_DAYS`, and
//! `TOKENLESS_STATS_TEXT_RETENTION_DAYS` override file config at runtime.
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
    /// keys are explicitly present in a valid on-disk config file.
    ///
    /// Returns `(false, false)` when the config file cannot be read or cannot
    /// be parsed as [`TokenlessConfig`] (matching [`Self::load`]'s fallback).
    pub fn file_retention_keys_present() -> (bool, bool) {
        let Ok(raw) = std::fs::read_to_string(Self::config_path()) else {
            return (false, false);
        };
        if serde_json::from_str::<TokenlessConfig>(&raw).is_err() {
            return (false, false);
        }
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(serde_json::Value::Object(map)) => (
                map.contains_key("stats_retention_days"),
                map.contains_key("stats_text_retention_days"),
            ),
            _ => (false, false),
        }
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
    /// Fast path: when all five settings are determined by env vars, the file
    /// read is skipped entirely — every setting is already determined, so
    /// opening the config file would have no effect. This avoids latency on
    /// slow or broken mounts in env-only deployments.
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
                stats_retention_days: None,
                stats_text_retention_days: None,
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
                stats_retention_days: None,
                stats_text_retention_days: None,
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

        // Fast path: all five settings are determined by env — no file read needed.
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
            };
        }

        let default_path = Self::config_path();
        let config_path = path.unwrap_or(&default_path);
        let base = read_file(config_path)
            .ok()
            .and_then(|s| serde_json::from_str::<TokenlessConfig>(&s).ok())
            .unwrap_or_default();

        let stats_enabled = stats_env.map_or(base.stats_enabled, parse_env_bool);
        let sls_enabled = sls_env.map_or(base.sls_enabled, parse_env_bool);
        let compression_enabled = compression_env.map_or(base.compression_enabled, parse_env_bool);
        let stats_retention_days = retention_env.unwrap_or_else(|| base.stats_retention_days());
        let stats_text_retention_days =
            text_retention_env.unwrap_or_else(|| base.stats_text_retention_days());

        Self {
            stats_enabled,
            sls_enabled,
            compression_enabled,
            stats_retention_days,
            stats_text_retention_days,
        }
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
