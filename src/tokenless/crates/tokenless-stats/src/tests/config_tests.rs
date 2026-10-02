#[test]
fn test_default_config() {
    let config = TokenlessConfig::default();
    assert!(config.is_stats_enabled());
}

#[test]
fn test_load_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let config = TokenlessConfig::load_with_env_and_path(None, Some(&path));
    assert!(config.is_stats_enabled());
}

#[test]
fn test_load_invalid_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let _ = std::fs::write(&path, "not json");
    let config = TokenlessConfig::load_with_env_and_path(None, Some(&path));
    assert!(config.is_stats_enabled());
}

#[test]
fn test_env_override_enabled() {
    let config = TokenlessConfig::load_with_env(Some("1"));
    assert!(config.is_stats_enabled());
}

#[test]
fn test_env_override_disabled() {
    let config = TokenlessConfig::load_with_env(Some("0"));
    assert!(!config.is_stats_enabled());
}

#[test]
fn test_env_override_true_string() {
    let config = TokenlessConfig::load_with_env(Some("true"));
    assert!(config.is_stats_enabled());
}

#[test]
fn test_env_override_overrides_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    // Write file config with stats_enabled=false
    let _ = std::fs::write(&path, "{\"stats_enabled\":false}");
    // Env override to enable
    let config = TokenlessConfig::load_with_env_and_path(Some("1"), Some(&path));
    assert!(config.is_stats_enabled());
}

#[test]
fn test_sls_enabled_default_true() {
    let config = TokenlessConfig::default();
    assert!(config.is_sls_enabled());
    assert!(config.is_stats_enabled());
}

#[test]
fn test_load_sls_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let config = TokenlessConfig::load_with_envs_and_path(None, None, None, Some(&path));
    assert!(config.is_sls_enabled());
    assert!(config.is_stats_enabled());
}

#[test]
fn test_sls_env_override_enabled() {
    let config = TokenlessConfig::load_with_envs(Some("1"), None);
    assert!(config.is_stats_enabled());
    assert!(config.is_sls_enabled());

    let config = TokenlessConfig::load_with_envs(None, Some("1"));
    assert!(config.is_stats_enabled());
    assert!(config.is_sls_enabled());
}

#[test]
fn test_sls_env_override_disabled() {
    // stats_env="0" disables stats, sls stays default true
    let config = TokenlessConfig::load_with_envs(Some("0"), None);
    assert!(!config.is_stats_enabled());
    assert!(config.is_sls_enabled());

    // sls_env="0" explicitly disables sls
    let config = TokenlessConfig::load_with_envs(None, Some("0"));
    assert!(config.is_stats_enabled());
    assert!(!config.is_sls_enabled());
}

#[test]
fn test_sls_env_override_overrides_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    // Write file config with sls_enabled=true
    let _ = std::fs::write(&path, "{\"stats_enabled\":true,\"sls_enabled\":true}");
    // Env override to disable sls
    let config = TokenlessConfig::load_with_envs_and_path(None, Some("0"), None, Some(&path));
    assert!(config.is_stats_enabled());
    assert!(!config.is_sls_enabled());
}

#[test]
fn test_both_env_overrides() {
    let config = TokenlessConfig::load_with_envs(Some("0"), Some("1"));
    assert!(!config.is_stats_enabled());
    assert!(config.is_sls_enabled());

    let config = TokenlessConfig::load_with_envs(Some("1"), Some("0"));
    assert!(config.is_stats_enabled());
    assert!(!config.is_sls_enabled());
}

#[test]
fn test_empty_env_treated_as_unset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    // File config has stats_enabled=true
    let _ = std::fs::write(&path, "{\"stats_enabled\":true}");
    // Empty string should fall through to file config (true), not override to false
    let config = TokenlessConfig::load_with_envs_and_path(Some(""), None, None, Some(&path));
    assert!(config.is_stats_enabled());
}

#[test]
fn test_compression_default_true() {
    let config = TokenlessConfig::default();
    assert!(config.is_compression_enabled());
}

#[test]
fn test_compression_env_override() {
    let config = TokenlessConfig::load_with_envs_and_path(None, None, Some("0"), None);
    assert!(!config.is_compression_enabled());

    let config = TokenlessConfig::load_with_envs_and_path(None, None, Some("1"), None);
    assert!(config.is_compression_enabled());
}

#[test]
fn test_compression_env_overrides_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let _ = std::fs::write(&path, "{\"compression_enabled\":false}");
    let config = TokenlessConfig::load_with_envs_and_path(None, None, Some("1"), Some(&path));
    assert!(config.is_compression_enabled());
}

#[test]
fn test_compression_file_config_honored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let _ = std::fs::write(&path, "{\"compression_enabled\":false}");
    let config = TokenlessConfig::load_with_envs_and_path(None, None, None, Some(&path));
    assert!(!config.is_compression_enabled());
}

#[test]
fn test_compression_empty_env_treated_as_unset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let _ = std::fs::write(&path, "{\"compression_enabled\":false}");
    let config = TokenlessConfig::load_with_envs_and_path(None, None, Some(""), Some(&path));
    assert!(!config.is_compression_enabled());
}

#[test]
fn test_compression_config_honored_with_stats_sls_envs() {
    // Regression: when both stats and sls envs are set, compression_enabled
    // must fall back to config.json (not default true) when its own env is
    // unset — otherwise a dry-run config is silently bypassed.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let _ = std::fs::write(
        &path,
        "{\"compression_enabled\":false,\"stats_enabled\":true,\"sls_enabled\":false}",
    );
    let config = TokenlessConfig::load_with_envs_and_path(
        Some("true"),
        Some("true"),
        None,
        Some(&path),
    );
    assert!(config.is_stats_enabled());
    assert!(config.is_sls_enabled());
    assert!(!config.is_compression_enabled());
}

#[test]
fn test_compression_config_honored_with_stats_sls_envs_unreadable_file() {
    // Regression guard: when stats/sls envs are set but compression_env is absent
    // and the config file is unreadable (e.g. nonexistent path), the loader must
    // still honor env > default — not silently bypass with a stale cached value.
    let path = std::path::PathBuf::from("/nonexistent/path/config.json");
    let config = TokenlessConfig::load_with_envs_and_path(
        Some("true"),
        Some("true"),
        None,
        Some(&path),
    );
    assert!(config.is_stats_enabled());
    assert!(config.is_sls_enabled());
    // compression_env unset + file unreadable → falls back to default (true)
    assert!(config.is_compression_enabled());
}

#[test]
fn test_all_envs_set_skips_file_read() {
    // Fast path: when all three envs are present, the config file must not be
    // read at all. Observe the read itself via an injected reader that records
    // invocations and returns values opposite to the envs — asserting only the
    // returned config is blind here, because env overrides every toggle even
    // when the slow path falls back to defaults after a failed read.
    let path = std::path::PathBuf::from("/nonexistent/path/config.json");
    let read_attempted = std::cell::Cell::new(false);
    let config = TokenlessConfig::load_with_envs_and_file_reader(
        Some("true"),
        Some("false"),
        Some("false"),
        Some(&path),
        |_p| {
            read_attempted.set(true);
            Ok(
                "{\"stats_enabled\":false,\"sls_enabled\":true,\"compression_enabled\":true}"
                    .to_string(),
            )
        },
    );
    assert!(
        !read_attempted.get(),
        "all-envs-set fast path must not read the config file"
    );
    assert!(config.is_stats_enabled());
    assert!(!config.is_sls_enabled());
    assert!(!config.is_compression_enabled());
}

#[test]
fn test_partial_envs_attempt_file_read() {
    // Companion guard for test_all_envs_set_skips_file_read: with one toggle's
    // env unset, the loader must attempt the file read to fill the gap. This
    // proves the injected reader actually observes reads, so the fast-path
    // test's !read_attempted assertion cannot pass vacuously.
    let path = std::path::PathBuf::from("/nonexistent/path/config.json");
    let read_attempted = std::cell::Cell::new(false);
    let config = TokenlessConfig::load_with_envs_and_file_reader(
        Some("true"),
        Some("true"),
        None,
        Some(&path),
        |_p| {
            read_attempted.set(true);
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "config missing",
            ))
        },
    );
    assert!(
        read_attempted.get(),
        "partial-env load must attempt to read the config file"
    );
    // Read failed → built-in default fills the gap: env > default preserved.
    assert!(config.is_stats_enabled());
    assert!(config.is_sls_enabled());
    assert!(config.is_compression_enabled());
}

#[test]
fn test_compression_env_overrides_file_with_stats_sls_envs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let _ = std::fs::write(&path, "{\"compression_enabled\":false}");
    let config = TokenlessConfig::load_with_envs_and_path(
        Some("true"),
        Some("true"),
        Some("true"),
        Some(&path),
    );
    assert!(config.is_compression_enabled());
}

#[test]
fn test_parse_env_bool_yes_variant() {
    let config = TokenlessConfig::load_with_env(Some("yes"));
    assert!(config.is_stats_enabled());
    let config = TokenlessConfig::load_with_env(Some("YES"));
    assert!(config.is_stats_enabled());
}

#[test]
fn test_parse_env_bool_false_string() {
    let config = TokenlessConfig::load_with_env(Some("false"));
    assert!(!config.is_stats_enabled());
}

#[test]
fn test_save_and_reload_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".tokenless/config.json");
    let config = TokenlessConfig {
        stats_enabled: false,
        sls_enabled: true,
        compression_enabled: false,
    };
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let json = serde_json::to_string_pretty(&config).unwrap();
    std::fs::write(&path, json).unwrap();

    let reloaded = TokenlessConfig::load_with_envs_and_path(None, None, None, Some(&path));
    assert!(!reloaded.stats_enabled);
    assert!(reloaded.sls_enabled);
    assert!(!reloaded.compression_enabled);
}

#[test]
fn test_default_all_enabled() {
    let config = TokenlessConfig::default();
    assert!(config.is_stats_enabled());
    assert!(config.is_sls_enabled());
    assert!(config.is_compression_enabled());
}

#[test]
fn test_serde_round_trip() {
    let config = TokenlessConfig {
        stats_enabled: false,
        sls_enabled: false,
        compression_enabled: true,
    };
    let json = serde_json::to_string(&config).unwrap();
    let deserialized: TokenlessConfig = serde_json::from_str(&json).unwrap();
    assert!(!deserialized.stats_enabled);
    assert!(!deserialized.sls_enabled);
    assert!(deserialized.compression_enabled);
}

static CONFIG_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ConfigPathEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev_stats: Option<std::ffi::OsString>,
    prev_sls: Option<std::ffi::OsString>,
    prev_compression: Option<std::ffi::OsString>,
}

impl ConfigPathEnvGuard {
    fn new(
        path: std::path::PathBuf,
        stats_env: &str,
        sls_env: &str,
        compression_env: &str,
    ) -> Self {
        let lock = CONFIG_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        TokenlessConfig::override_config_path_for_tests(Some(path));
        let prev_stats = std::env::var_os("TOKENLESS_STATS_ENABLED");
        let prev_sls = std::env::var_os("TOKENLESS_SLS_ENABLED");
        let prev_compression = std::env::var_os("TOKENLESS_COMPRESSION_ENABLED");
        unsafe {
            std::env::set_var("TOKENLESS_STATS_ENABLED", stats_env);
            std::env::set_var("TOKENLESS_SLS_ENABLED", sls_env);
            std::env::set_var("TOKENLESS_COMPRESSION_ENABLED", compression_env);
        }
        Self {
            _lock: lock,
            prev_stats,
            prev_sls,
            prev_compression,
        }
    }
}

impl Drop for ConfigPathEnvGuard {
    fn drop(&mut self) {
        TokenlessConfig::override_config_path_for_tests(None);
        unsafe {
            match &self.prev_stats {
                Some(v) => std::env::set_var("TOKENLESS_STATS_ENABLED", v),
                None => std::env::remove_var("TOKENLESS_STATS_ENABLED"),
            }
            match &self.prev_sls {
                Some(v) => std::env::set_var("TOKENLESS_SLS_ENABLED", v),
                None => std::env::remove_var("TOKENLESS_SLS_ENABLED"),
            }
            match &self.prev_compression {
                Some(v) => std::env::set_var("TOKENLESS_COMPRESSION_ENABLED", v),
                None => std::env::remove_var("TOKENLESS_COMPRESSION_ENABLED"),
            }
        }
    }
}

#[test]
fn load_from_file_ignores_would_be_env_overrides() {
    // `stats enable`/`disable` persist from this snapshot. A session
    // TOKENLESS_COMPRESSION_ENABLED=0 A/B run must not rewrite the file's
    // compression toggle when the user only asked to flip stats recording.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        "{\"stats_enabled\":false,\"sls_enabled\":true,\"compression_enabled\":true}",
    )
    .unwrap();
    let _guard = ConfigPathEnvGuard::new(path, "1", "0", "0");

    let runtime = TokenlessConfig::load();
    assert!(runtime.is_stats_enabled());
    assert!(!runtime.is_sls_enabled());
    assert!(!runtime.is_compression_enabled());

    let persist = TokenlessConfig::load_from_file();
    assert!(!persist.is_stats_enabled());
    assert!(persist.is_sls_enabled());
    assert!(persist.is_compression_enabled());

    let json = serde_json::to_value(&persist).unwrap();
    assert_eq!(json["stats_enabled"], false);
    assert_eq!(json["sls_enabled"], true);
    assert_eq!(json["compression_enabled"], true);
}

#[test]
fn stats_disable_persist_keeps_file_compression_and_sls() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        "{\"stats_enabled\":true,\"sls_enabled\":false,\"compression_enabled\":false}",
    )
    .unwrap();
    let _guard = ConfigPathEnvGuard::new(path, "0", "1", "1");

    let runtime = TokenlessConfig::load();
    assert!(!runtime.is_stats_enabled());
    assert!(runtime.is_sls_enabled());
    assert!(runtime.is_compression_enabled());

    let persist = TokenlessConfig::load_from_file();
    assert!(persist.is_stats_enabled());
    assert!(!persist.is_sls_enabled());
    assert!(!persist.is_compression_enabled());
}
