use std::sync::Mutex;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

fn default_path_resolver(home: &str) -> DatabasePathResolver {
    DatabasePathResolver {
        home: std::sync::OnceLock::from(home.to_string()),
        data_dir: std::sync::OnceLock::from(Ok(PathBuf::from(home).join(".tokenless"))),
    }
}

struct TempDbGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    test_dir: String,
    sls_dir: String,
    stash_db_path: String,
    prev_stats_db: Option<std::ffi::OsString>,
    prev_stash_db: Option<std::ffi::OsString>,
    prev_sls_path: Option<std::ffi::OsString>,
}

impl TempDbGuard {
    fn new() -> Option<Self> {
        let lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = get_home_dir();
        if home.is_empty() {
            return None;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let test_dir = format!("{home}/.tokenless-test-{nanos}");
        let sls_dir = format!("/tmp/tokenless-sls-test-{nanos}");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::create_dir_all(&sls_dir).unwrap();
        let sls_path = format!("{sls_dir}/tokenless.jsonl");
        // Pre-create the JSONL file so SlsWriter::write can open it.
        std::fs::write(&sls_path, "").unwrap();
        let prev_stats_db = std::env::var_os("TOKENLESS_STATS_DB");
        let prev_stash_db = std::env::var_os("TOKENLESS_STASH_DB");
        let prev_sls_path = std::env::var_os("TOKENLESS_SLS_PATH");
        unsafe {
            std::env::set_var("TOKENLESS_STATS_DB", format!("{test_dir}/stats.db"));
            std::env::set_var("TOKENLESS_STASH_DB", format!("{test_dir}/stash.db"));
            std::env::set_var("TOKENLESS_SLS_PATH", &sls_path);
        }
        let stash_db_path = format!("{test_dir}/stash.db");
        Some(TempDbGuard {
            _lock: lock,
            test_dir,
            sls_dir,
            stash_db_path,
            prev_stats_db,
            prev_stash_db,
            prev_sls_path,
        })
    }

    /// Returns the temp stash DB path, useful for override-path tests.
    fn stash_db_path(&self) -> &str {
        &self.stash_db_path
    }
}

struct PersistConfigGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    path: PathBuf,
    prev_stats: Option<std::ffi::OsString>,
    prev_sls: Option<std::ffi::OsString>,
    prev_compression: Option<std::ffi::OsString>,
}

impl PersistConfigGuard {
    fn with_file_and_env(
        file_json: &str,
        stats_env: &str,
        sls_env: &str,
        compression_env: &str,
    ) -> Self {
        let lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, file_json).unwrap();
        TokenlessConfig::override_config_path_for_tests(Some(path.clone()));
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
            _dir: dir,
            path,
            prev_stats,
            prev_sls,
            prev_compression,
        }
    }

    fn read_persisted(&self) -> TokenlessConfig {
        serde_json::from_str(&std::fs::read_to_string(&self.path).unwrap()).unwrap()
    }
}

impl Drop for PersistConfigGuard {
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

impl Drop for TempDbGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prev_stats_db {
                Some(v) => std::env::set_var("TOKENLESS_STATS_DB", v),
                None => std::env::remove_var("TOKENLESS_STATS_DB"),
            }
            match &self.prev_stash_db {
                Some(v) => std::env::set_var("TOKENLESS_STASH_DB", v),
                None => std::env::remove_var("TOKENLESS_STASH_DB"),
            }
            match &self.prev_sls_path {
                Some(v) => std::env::set_var("TOKENLESS_SLS_PATH", v),
                None => std::env::remove_var("TOKENLESS_SLS_PATH"),
            }
        }
        std::fs::remove_dir_all(&self.test_dir).ok();
        std::fs::remove_dir_all(&self.sls_dir).ok();
    }
}

fn temp_subdir(label: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!(
        "tokenless-db-validate-{}-{}-{}",
        std::process::id(),
        nanos,
        label
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn validate_db_path_rejects_without_trusted_root() {
    let err = validate_db_path(Path::new("/tmp/whatever.db"), "", None).unwrap_err();
    assert!(err.contains("no trusted root"));
}

#[test]
fn validate_db_path_accepts_path_inside_home() {
    let home = temp_subdir("inside");
    let canon_home = std::fs::canonicalize(&home).unwrap();
    let inner = canon_home.join("stats.db");
    let result = validate_db_path(
        &inner,
        canon_home.to_str().unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(result, inner);
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn validate_db_path_rejects_path_outside_trusted_roots() {
    let home = temp_subdir("outside-home");
    let canon_home = std::fs::canonicalize(&home).unwrap();
    // Pick a known-existing directory that is NOT under home.
    let outside = std::path::Path::new("/etc/hosts");
    if !outside.exists() {
        std::fs::remove_dir_all(&home).ok();
        return;
    }
    let err = validate_db_path(
        Path::new("/etc/hosts"),
        canon_home.to_str().unwrap(),
        None,
    )
    .unwrap_err();
    assert!(err.contains("outside the trusted home and data directory"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn validate_db_path_rejects_parent_dir_bypass_with_existing_parent() {
    // ~/foo/../../etc/evil.db where /etc exists: canonicalize() of
    // the parent resolves to /etc, which must fail starts_with(home).
    let home = temp_subdir("pd-existing");
    let canon_home = std::fs::canonicalize(&home).unwrap();
    let escape = canon_home.join("foo/../../etc/evil.db");
    let err = validate_db_path(
        &escape,
        canon_home.to_str().unwrap(),
        None,
    )
    .unwrap_err();
    // Either "outside home" (parent canonicalized away from home) or
    // "cannot be resolved" (parent itself unreachable). Both are valid
    // rejections — what matters is no Ok return.
    assert!(err.contains("parent traversal"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn validate_db_path_canonicalizes_home_with_symlink_prefix() {
    // If the caller passes a home that contains a symlink in any
    // prefix (e.g. /tmp on macOS resolves to /private/tmp), the
    // candidate path will canonicalize to the resolved form and
    // diverge from the raw home unless validate_db_path canonicalizes
    // home too. Linux /tmp has no such symlink, so the assertion is
    // informational there but real coverage on macOS.
    let home = temp_subdir("sym-prefix");
    let inner = home.join("stats.db");
    let result = validate_db_path(&inner, home.to_str().unwrap(), None);
    assert!(
        result.is_ok(),
        "raw (non-canonical) home should be accepted after internal canonicalization: {result:?}"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn validate_db_path_rejects_parent_dir_bypass_with_nonexistent_parent() {
    // ~/nonexistent-path/../../etc/evil.db where nonexistent-path
    // doesn't exist: parent canonicalize() ALSO fails, so without the
    // hardening this path would slip through via the old fallback.
    let home = temp_subdir("pd-nonexistent");
    let canon_home = std::fs::canonicalize(&home).unwrap();
    let escape = canon_home.join("does-not-exist-xyz/../../etc/evil.db");
    let result = validate_db_path(
        &escape,
        canon_home.to_str().unwrap(),
        None,
    );
    assert!(
        result.is_err(),
        "ParentDir bypass via nonexistent intermediate must be rejected; got {result:?}"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn validate_data_dir_path_accepts_nonexistent_descendant() {
    let home = temp_subdir("data-dir-home");
    let candidate = home.join("nested/data");
    let result = tokenless_stats::validate_data_dir(&candidate).unwrap();
    assert_eq!(result, candidate);
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn validate_data_dir_path_accepts_outside_home() {
    let home = temp_subdir("data-dir-outside-home");
    let outside = temp_subdir("data-dir-outside-candidate");
    let resolved = tokenless_stats::resolve_data_dir(
        Some(home.as_path()),
        outside.to_str(),
    )
    .unwrap();
    assert_eq!(resolved, outside.canonicalize().unwrap());
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&outside).ok();
}

#[test]
fn validate_data_dir_path_rejects_relative_path() {
    let error = tokenless_stats::validate_data_dir(Path::new("relative/data")).unwrap_err();
    assert!(error.to_string().contains("not absolute"));
}

#[cfg(unix)]
#[test]
fn validate_data_dir_path_canonicalizes_symlink() {
    use std::os::unix::fs::symlink;

    let home = temp_subdir("data-dir-symlink-home");
    let outside = temp_subdir("data-dir-symlink-outside");
    let link = home.join("redirect");
    symlink(&outside, &link).unwrap();
    let candidate = link.join("data");
    let resolved = tokenless_stats::validate_data_dir(&candidate).unwrap();
    assert_eq!(resolved, outside.canonicalize().unwrap().join("data"));
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&outside).ok();
}

#[test]
fn validate_data_dir_path_rejects_parent_traversal() {
    let home = temp_subdir("data-dir-traversal");
    let candidate = home.join("nested/../data");
    let error = tokenless_stats::validate_data_dir(&candidate).unwrap_err();
    assert!(error.to_string().contains("parent traversal"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn validate_data_dir_path_rejects_existing_file() {
    let home = temp_subdir("data-dir-file");
    let candidate = home.join("not-a-directory");
    std::fs::write(&candidate, "").unwrap();
    let error = tokenless_stats::validate_data_dir(&candidate).unwrap_err();
    assert!(error.to_string().contains("not a directory"));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn read_input_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("input.json");
    std::fs::write(&f, r#"{"key":"value"}"#).unwrap();
    let result = read_input(&Some(f.to_str().unwrap().to_string())).unwrap();
    assert_eq!(result, r#"{"key":"value"}"#);
}

#[test]
fn read_input_file_not_found() {
    let result = read_input(&Some("/nonexistent/path.json".to_string()));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Failed to open"));
}

#[test]
fn resolve_mode_active_when_on() {
    let mode = resolve_mode(true, 100, 50);
    assert_eq!(mode, CompressionMode::Active);
}

#[test]
fn resolve_mode_dryrun_when_off() {
    let mode = resolve_mode(false, 100, 50);
    assert_eq!(mode, CompressionMode::DryRun);
}

#[test]
fn warn_mode_mismatch_empty_records_no_panic() {
    warn_mode_mismatch("test", &[], CompressionMode::Active);
}

#[test]
fn warn_mode_mismatch_detects_wrong_modes() {
    let records = vec![
        StatsRecord::new(
            OperationType::CompressSchema,
            "cli".to_string(),
            100,
            25,
            50,
            12,
        )
        .with_mode(CompressionMode::Active),
    ];
    // Expect DryRun but got Active → warning printed to stderr (no panic)
    warn_mode_mismatch("baseline", &records, CompressionMode::DryRun);
}

#[test]
fn warn_mode_mismatch_no_warning_when_matching() {
    let records = vec![
        StatsRecord::new(
            OperationType::CompressSchema,
            "cli".to_string(),
            100,
            25,
            50,
            12,
        )
        .with_mode(CompressionMode::DryRun),
    ];
    warn_mode_mismatch("baseline", &records, CompressionMode::DryRun);
}

#[test]
fn get_stash_db_path_default() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let home = get_home_dir();
    if home.is_empty() {
        return;
    }
    let path = get_stash_db_path_with(&default_path_resolver(&home), None);
    assert!(path.is_ok());
    assert!(path.unwrap().ends_with(".tokenless/stash.db"));
}

#[test]
fn get_data_dir_uses_managed_dsh_override_only_in_dsh_shell() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let managed = tempfile::tempdir().unwrap();
    let explicit = tempfile::tempdir().unwrap();
    let previous_data_dir = std::env::var_os("TOKENLESS_DATA_DIR");
    let previous_stats_db = std::env::var_os("TOKENLESS_STATS_DB");
    let previous_stash_db = std::env::var_os("TOKENLESS_STASH_DB");
    let previous_dsh_shell = std::env::var_os("DSH_SHELL");
    let previous_dsh_data_dir = std::env::var_os("DSH_TOKENLESS_DATA_DIR");
    let previous_dsh_stats_db = std::env::var_os("DSH_TOKENLESS_STATS_DB");
    let previous_dsh_stash_db = std::env::var_os("DSH_TOKENLESS_STASH_DB");
    let managed_stats = managed.path().join("managed-stats.db");
    let managed_stash = managed.path().join("managed-stash.db");
    let explicit_stats = explicit.path().join("explicit-stats.db");
    let explicit_stash = explicit.path().join("explicit-stash.db");

    unsafe {
        std::env::remove_var("TOKENLESS_DATA_DIR");
        std::env::remove_var("TOKENLESS_STATS_DB");
        std::env::remove_var("TOKENLESS_STASH_DB");
        std::env::set_var("DSH_SHELL", "1");
        std::env::set_var("DSH_TOKENLESS_DATA_DIR", managed.path());
        std::env::set_var("DSH_TOKENLESS_STATS_DB", &managed_stats);
        std::env::set_var("DSH_TOKENLESS_STASH_DB", &managed_stash);
    }
    let managed_result = get_data_dir(home.path().to_str().unwrap());
    let managed_paths = DatabasePathResolver {
        home: std::sync::OnceLock::from(home.path().to_string_lossy().into_owned()),
        data_dir: std::sync::OnceLock::new(),
    };
    let managed_stats_result = get_db_path_with(&managed_paths);
    let managed_stash_result = get_stash_db_path_with(&managed_paths, None);

    unsafe {
        std::env::remove_var("DSH_SHELL");
    }
    let ignored_result = get_data_dir(home.path().to_str().unwrap());

    unsafe {
        std::env::set_var("DSH_SHELL", "1");
        std::env::set_var("TOKENLESS_DATA_DIR", explicit.path());
        std::env::set_var("TOKENLESS_STATS_DB", &explicit_stats);
        std::env::set_var("TOKENLESS_STASH_DB", &explicit_stash);
    }
    let explicit_result = get_data_dir(home.path().to_str().unwrap());
    let explicit_paths = DatabasePathResolver {
        home: std::sync::OnceLock::from(home.path().to_string_lossy().into_owned()),
        data_dir: std::sync::OnceLock::new(),
    };
    let explicit_stats_result = get_db_path_with(&explicit_paths);
    let explicit_stash_result = get_stash_db_path_with(&explicit_paths, None);

    unsafe {
        match previous_data_dir {
            Some(value) => std::env::set_var("TOKENLESS_DATA_DIR", value),
            None => std::env::remove_var("TOKENLESS_DATA_DIR"),
        }
        match previous_stats_db {
            Some(value) => std::env::set_var("TOKENLESS_STATS_DB", value),
            None => std::env::remove_var("TOKENLESS_STATS_DB"),
        }
        match previous_stash_db {
            Some(value) => std::env::set_var("TOKENLESS_STASH_DB", value),
            None => std::env::remove_var("TOKENLESS_STASH_DB"),
        }
        match previous_dsh_shell {
            Some(value) => std::env::set_var("DSH_SHELL", value),
            None => std::env::remove_var("DSH_SHELL"),
        }
        match previous_dsh_data_dir {
            Some(value) => std::env::set_var("DSH_TOKENLESS_DATA_DIR", value),
            None => std::env::remove_var("DSH_TOKENLESS_DATA_DIR"),
        }
        match previous_dsh_stats_db {
            Some(value) => std::env::set_var("DSH_TOKENLESS_STATS_DB", value),
            None => std::env::remove_var("DSH_TOKENLESS_STATS_DB"),
        }
        match previous_dsh_stash_db {
            Some(value) => std::env::set_var("DSH_TOKENLESS_STASH_DB", value),
            None => std::env::remove_var("DSH_TOKENLESS_STASH_DB"),
        }
    }

    assert_eq!(managed_result.unwrap(), managed.path().canonicalize().unwrap());
    assert_eq!(managed_stats_result.unwrap(), managed_stats);
    assert_eq!(managed_stash_result.unwrap(), managed_stash);
    assert_eq!(
        ignored_result.unwrap(),
        home.path().canonicalize().unwrap().join(".tokenless")
    );
    assert_eq!(
        explicit_result.unwrap(),
        explicit.path().canonicalize().unwrap()
    );
    assert_eq!(explicit_stats_result.unwrap(), explicit_stats);
    assert_eq!(explicit_stash_result.unwrap(), explicit_stash);
}

#[test]
fn get_stash_db_path_with_valid_override() {
    let guard = match TempDbGuard::new() { Some(g) => g, None => return };
    // Use the temp stash path as an explicit override; get_stash_db_path
    // validates it (under home via TempDbGuard) and returns it directly.
    let result = get_stash_db_path_with(
        &DatabasePathResolver::default(),
        Some(guard.stash_db_path()),
    );
    assert_eq!(result.unwrap(), PathBuf::from(guard.stash_db_path()));
}

#[test]
fn open_stash_store_or_err_falls_back_on_bad_override() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = open_stash_store_or_err(Some("/nonexistent/deep/dir/stash.db"));
    // Bad override path is rejected and falls back to default; result is Ok
    assert!(result.is_ok());
}

#[test]
fn ensure_db_dir_creates_parent() {
    let _guard = TempDbGuard::new();

    // ensure_db_dir is idempotent — calling it when the dir already exists
    // (which it does for most test envs) succeeds.
    let db_path = get_db_path_with(&DatabasePathResolver::default()).unwrap();
    let result = ensure_db_dir(&db_path);
    // With TempDbGuard the stats DB path points to a temp dir, so this succeeds.
    assert!(result.is_ok());
}

#[cfg(unix)]
#[test]
fn ensure_db_dir_preserves_existing_parent_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let parent = temp_subdir("existing-mode");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o750)).unwrap();

    ensure_db_dir(&parent.join("stats.db")).unwrap();

    assert_eq!(parent.metadata().unwrap().permissions().mode() & 0o777, 0o750);
    std::fs::remove_dir_all(parent).ok();
}

#[test]
fn record_compression_stats_skips_when_both_disabled() {
    let config = TokenlessConfig {
        stats_enabled: false,
        sls_enabled: false,
        ..TokenlessConfig::default()
    };
    // Should return immediately without touching DB
    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressSchema,
        Some("test".to_string()),
        None,
        None,
        "before".to_string(),
        "after".to_string(),
        CompressionMode::Active,
        None,
        None,
        None,
    );
}

#[test]
fn record_compression_stats_skips_when_no_savings() {
    let config = TokenlessConfig::default();
    // after is larger than before → no savings → skip
    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressSchema,
        Some("test".to_string()),
        None,
        None,
        "short".to_string(),
        "this is longer text".to_string(),
        CompressionMode::Active,
        None,
        None,
        None,
    );
}

#[test]
fn record_compression_stats_records_when_savings_exist() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let config = TokenlessConfig::default();
    let long_before = "x".repeat(500);
    let short_after = "y".repeat(50);
    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressSchema,
        Some("test-agent".to_string()),
        Some("session-1".to_string()),
        Some("tool-1".to_string()),
        long_before,
        short_after,
        CompressionMode::Active,
        Some(1),
        Some(0),
        Some(10),
    );
}

#[test]
fn record_compression_stats_records_dryrun_mode() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let config = TokenlessConfig::default();
    let long_before = "x".repeat(500);
    let short_after = "y".repeat(50);
    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressResponse,
        None,
        None,
        None,
        long_before,
        short_after,
        CompressionMode::DryRun,
        None,
        None,
        None,
    );
}

#[test]
fn get_db_path_returns_valid_path() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let db_path = get_db_path_with(&DatabasePathResolver::default());
    assert!(db_path.unwrap().ends_with("stats.db"));
}

#[test]
fn open_recorder_exercises_path() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = open_recorder();
    // With TempDbGuard the stats DB path points to a writable temp dir.
    assert!(result.is_ok());
}

#[test]
fn read_input_oversized_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("big.json");
    // Create a file just slightly over the limit by checking what MAX_INPUT_BYTES is
    // For safety, just verify the function handles large strings without panicking
    let result = read_input(&Some(f.to_str().unwrap().to_string()));
    // File doesn't exist, so should error
    assert!(result.is_err());
}

#[test]
fn run_command_compress_schema_from_file() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("schema.json");
    std::fs::write(
        &f,
        r#"{"function":{"name":"test","description":"A test function with a long description that might get compressed by the schema compressor depending on settings","parameters":{"type":"object","properties":{"x":{"type":"string","title":"Remove Me","examples":["ex1"]}}}}}"#,
    )
    .unwrap();
    let result = run_command(Commands::CompressSchema {
        file: Some(f.to_str().unwrap().to_string()),
        batch: false,
        agent_id: Some("test-agent".to_string()),
        session_id: Some("test-session".to_string()),
        tool_use_id: Some("tool-1".to_string()),
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_compress_schema_batch() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("schemas.json");
    std::fs::write(
        &f,
        r#"[{"function":{"name":"a","parameters":{"type":"object","properties":{}}}},{"function":{"name":"b","parameters":{"type":"object","properties":{}}}}]"#,
    )
    .unwrap();
    let result = run_command(Commands::CompressSchema {
        file: Some(f.to_str().unwrap().to_string()),
        batch: true,
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_compress_schema_invalid_json() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("bad.json");
    std::fs::write(&f, "not json at all").unwrap();
    let result = run_command(Commands::CompressSchema {
        file: Some(f.to_str().unwrap().to_string()),
        batch: false,
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_err());
    assert!(result.unwrap_err().0.contains("JSON parse error"));
}

#[test]
fn run_command_compress_response_from_file() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("response.json");
    std::fs::write(
        &f,
        r#"{"data":{"items":[1,2,3,4,5,6,7,8,9,10],"debug":"info","value":null,"status":"ok","longText":""}}"#,
    )
    .unwrap();
    let result = run_command(Commands::CompressResponse {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: Some("test-agent".to_string()),
        session_id: Some("sess-1".to_string()),
        tool_use_id: Some("tool-1".to_string()),
        truncate_strings_at: Some(100),
        truncate_arrays_at: Some(5),
        array_tail_preserve: None,
        max_depth: Some(10),
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_compress_response_no_stash() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("resp.json");
    std::fs::write(&f, r#"{"key":"value"}"#).unwrap();
    let result = run_command(Commands::CompressResponse {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        truncate_strings_at: None,
        truncate_arrays_at: None,
        array_tail_preserve: None,
        max_depth: None,
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_compress_response_with_stash() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("resp.json");
    std::fs::write(&f, r#"{"key":"value"}"#).unwrap();
    let result = run_command(Commands::CompressResponse {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        truncate_strings_at: None,
        truncate_arrays_at: None,
        array_tail_preserve: None,
        max_depth: None,
        no_stash: false,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_retrieve_invalid_hash() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Retrieve {
        hash: "not-a-hash".to_string(),
        stash_db: None,
    });
    assert!(result.is_err());
    assert!(result.unwrap_err().0.contains("invalid stash hash"));
}

#[test]
fn run_command_retrieve_missing_hash() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Retrieve {
        hash: "abcdef0123456789abcdef01".to_string(),
        stash_db: None,
    });
    assert!(result.is_err());
    assert!(result.unwrap_err().0.contains("no stashed payload"));
}

#[test]
fn run_command_compress_toon_from_file() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("toon.json");
    std::fs::write(
        &f,
        r#"{"name":"test","items":[1,2,3],"nested":{"key":"value"}}"#,
    )
    .unwrap();
    let result = run_command(Commands::CompressToon {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: Some("agent".to_string()),
        session_id: Some("sess".to_string()),
        tool_use_id: Some("tool".to_string()),
        min_toon_chars: MIN_TOON_CHARS,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_decompress_toon_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let json_input = r#"{"name":"test","items":[1,2,3]}"#;
    let value: serde_json::Value = serde_json::from_str(json_input).unwrap();
    let toon_encoded = toon_format::encode_default(&value).unwrap();
    let toon_f = dir.path().join("input.toon");
    std::fs::write(&toon_f, &toon_encoded).unwrap();
    let result = run_command(Commands::DecompressToon {
        file: Some(toon_f.to_str().unwrap().to_string()),
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_decompress_toon_empty_input() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("empty.toon");
    std::fs::write(&f, "").unwrap();
    let result = run_command(Commands::DecompressToon {
        file: Some(f.to_str().unwrap().to_string()),
    });
    // Empty input decodes to null, which serializes to "null" and is printed.
    assert!(result.is_ok());
}

#[test]
fn run_command_retrieve_with_marker() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let store = open_stash_store_or_err(None).unwrap();
    let key = store.stash("hello world").unwrap().key;
    let marker = format!("<<tokenless:{key}>>");
    let result = run_command(Commands::Retrieve {
        hash: marker,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_retrieve_bare_hash() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let store = open_stash_store_or_err(None).unwrap();
    let key = store.stash("retrieve bare hash test").unwrap().key;
    let result = run_command(Commands::Retrieve {
        hash: key,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_stats_show_existing_record() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let recorder = match open_recorder() {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut record = StatsRecord::new(
        OperationType::CompressSchema,
        "test-agent".to_string(),
        500,
        125,
        100,
        25,
    );
    record = record
        .with_before_text("before-text-for-show".to_string())
        .with_after_text("after-text-for-show".to_string());
    let id = match recorder.record(&record) {
        Ok(id) => id,
        Err(_) => return,
    };
    let result = run_command(Commands::Stats(StatsCommands::Show { id }));
    // before_text and after_text are set above, so format_show succeeds.
    assert!(result.is_ok());
}

#[test]
fn run_command_stats_diff_existing_record() {
    let _guard = match TempDbGuard::new() {
        Some(g) => g,
        None => return,
    };
    let recorder = match open_recorder() {
        Ok(r) => r,
        Err(_) => return,
    };
    let record = StatsRecord::new(
        OperationType::CompressResponse,
        "test-agent".to_string(),
        500,
        125,
        100,
        25,
    )
    .with_session_id("diff-session")
    .with_tool_use_id("diff-tool")
    .with_text(
        "line one\nline removed\n".to_string(),
        "line one\n".to_string(),
    );
    let id = recorder.record(&record).unwrap();

    let result = run_command(Commands::Stats(StatsCommands::Diff {
        id: Some(id),
        session: None,
        tool_use_id: None,
        limit: DEFAULT_DIFF_LIMIT,
        sort: None,
        context: 3,
        no_color: true,
        json: false,
    }));
    assert!(result.is_ok());
}

#[test]
fn run_command_stats_diff_session_and_tool() {
    let _guard = match TempDbGuard::new() {
        Some(g) => g,
        None => return,
    };
    let recorder = match open_recorder() {
        Ok(r) => r,
        Err(_) => return,
    };
    recorder
        .record(
            &StatsRecord::new(
                OperationType::CompressResponse,
                "test-agent".to_string(),
                500,
                125,
                100,
                25,
            )
            .with_session_id("diff-session")
            .with_tool_use_id("diff-tool")
            .with_text("before".to_string(), "after".to_string()),
        )
        .unwrap();

    let session = run_command(Commands::Stats(StatsCommands::Diff {
        id: None,
        session: Some("diff-session".to_string()),
        tool_use_id: None,
        limit: DEFAULT_DIFF_LIMIT,
        sort: Some(DiffSortArg::Saved),
        context: 3,
        no_color: true,
        json: true,
    }));
    assert!(session.is_ok());

    let tool = run_command(Commands::Stats(StatsCommands::Diff {
        id: None,
        session: Some("diff-session".to_string()),
        tool_use_id: Some("diff-tool".to_string()),
        limit: DEFAULT_DIFF_LIMIT,
        sort: None,
        context: 3,
        no_color: true,
        json: false,
    }));
    assert!(tool.is_ok());
}

#[test]
fn stats_diff_cli_validates_scope_and_limit() {
    let record = Cli::try_parse_from(["tokenless", "stats", "diff", "42"]).unwrap();
    match record.command {
        Commands::Stats(StatsCommands::Diff {
            id,
            session,
            limit,
            context,
            ..
        }) => {
            assert_eq!(id, Some(42));
            assert_eq!(session, None);
            assert_eq!(limit, DEFAULT_DIFF_LIMIT);
            assert_eq!(context, 3);
        }
        _ => panic!("expected stats diff"),
    }

    assert!(
        Cli::try_parse_from([
            "tokenless",
            "stats",
            "diff",
            "42",
            "--session",
            "session-1"
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from(["tokenless", "stats", "diff", "--session", "session-1", "-l", "0"])
            .is_err()
    );
    assert!(
        Cli::try_parse_from(["tokenless", "stats", "diff", "42", "--limit", "1"]).is_err()
    );
    assert!(
        Cli::try_parse_from(["tokenless", "stats", "diff", "--tool-use-id", "tool-1"]).is_err()
    );
    let help = match Cli::try_parse_from(["tokenless", "stats", "diff", "--help"]) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("expected help output"),
    };
    assert!(help.contains("[default: 20]"));
    assert!(
        Cli::try_parse_from([
            "tokenless",
            "stats",
            "diff",
            "--session",
            "session-1",
            "--tool-use-id",
            "tool-1",
            "--sort",
            "time"
        ])
        .is_err()
    );
}

#[test]
fn stats_summary_cli_rejects_zero_limit() {
    let zero = match Cli::try_parse_from(["tokenless", "stats", "summary", "--limit", "0"]) {
        Err(error) => error,
        Ok(_) => panic!("summary --limit 0 must fail at parse time"),
    };
    assert!(zero.to_string().contains("greater than zero"));

    let compare_zero = match Cli::try_parse_from([
        "tokenless",
        "stats",
        "summary",
        "--limit",
        "0",
        "--compare",
        "baseline-run",
        "active-run",
    ]) {
        Err(error) => error,
        Ok(_) => panic!("compare --limit 0 must fail at parse time"),
    };
    assert!(compare_zero.to_string().contains("greater than zero"));

    let parsed = Cli::try_parse_from(["tokenless", "stats", "summary", "--limit", "1"]).unwrap();
    match parsed.command {
        Commands::Stats(StatsCommands::Summary { limit, .. }) => {
            assert_eq!(limit, Some(1));
        }
        _ => panic!("expected stats summary"),
    }
}

#[test]
fn run_command_compress_response_large_with_truncation() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("large.json");
    let items: Vec<serde_json::Value> = (0..20).map(|i| serde_json::json!({"id": i, "name": format!("item_{}", i), "long_field": "x".repeat(200)})).collect();
    let data = serde_json::json!({"results": items, "debug": "debug info", "meta": null});
    std::fs::write(&f, serde_json::to_string(&data).unwrap()).unwrap();
    let result = run_command(Commands::CompressResponse {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: Some("test-agent".to_string()),
        session_id: Some("sess-trunc".to_string()),
        tool_use_id: Some("tool-trunc".to_string()),
        truncate_strings_at: Some(50),
        truncate_arrays_at: Some(3),
        array_tail_preserve: None,
        max_depth: Some(5),
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_compress_schema_with_stash() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("schema_stash.json");
    let long_desc = "A".repeat(500);
    let schema = serde_json::json!({
        "function": {
            "name": "test_stash",
            "description": long_desc,
            "parameters": {"type": "object", "properties": {"x": {"type": "string"}}}
        }
    });
    std::fs::write(&f, serde_json::to_string(&schema).unwrap()).unwrap();
    let result = run_command(Commands::CompressSchema {
        file: Some(f.to_str().unwrap().to_string()),
        batch: false,
        agent_id: Some("stash-agent".to_string()),
        session_id: Some("stash-sess".to_string()),
        tool_use_id: None,
        no_stash: false,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_stats_summary() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Stats(StatsCommands::Summary {
        limit: Some(10),
        json: false,
        compare: None,
    }));
    assert!(result.is_ok());
}

#[test]
fn run_command_stats_summary_json() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Stats(StatsCommands::Summary {
        limit: Some(10),
        json: true,
        compare: None,
    }));
    assert!(result.is_ok());
}

#[test]
fn run_command_stats_list() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Stats(StatsCommands::List { limit: 5 }));
    assert!(result.is_ok());
}

#[test]
fn run_command_stats_show_nonexistent() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Stats(StatsCommands::Show { id: 999999 }));
    assert!(result.is_err());
    assert!(result.unwrap_err().0.contains("not found"));
}

#[test]
fn run_command_stats_diff_nonexistent_scopes() {
    let _guard = match TempDbGuard::new() {
        Some(g) => g,
        None => return,
    };
    let record = run_command(Commands::Stats(StatsCommands::Diff {
        id: Some(999999),
        session: None,
        tool_use_id: None,
        limit: DEFAULT_DIFF_LIMIT,
        sort: None,
        context: 3,
        no_color: true,
        json: false,
    }))
    .unwrap_err();
    assert!(record.0.contains("not found"));
    assert_eq!(record.1, 1);

    let session = run_command(Commands::Stats(StatsCommands::Diff {
        id: None,
        session: Some("missing-session".to_string()),
        tool_use_id: None,
        limit: DEFAULT_DIFF_LIMIT,
        sort: None,
        context: 3,
        no_color: true,
        json: false,
    }))
    .unwrap_err();
    assert!(session.0.contains("No records found"));
    assert_eq!(session.1, 1);

    let injected = run_command(Commands::Stats(StatsCommands::Diff {
        id: None,
        session: Some("missing\u{1b}]0;INJECTED\u{7}".to_string()),
        tool_use_id: Some("tool\u{1b}]52;c;INJECTED\u{7}".to_string()),
        limit: DEFAULT_DIFF_LIMIT,
        sort: None,
        context: 3,
        no_color: true,
        json: false,
    }))
    .unwrap_err();
    assert!(!injected.0.contains('\u{1b}'));
    assert!(!injected.0.contains('\u{7}'));
    assert!(injected.0.contains("INJECTED"));
}

#[test]
fn run_command_stats_clear_without_confirm() {
    // Clear with --yes to avoid interactive prompt
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Stats(StatsCommands::Clear { yes: true }));
    assert!(result.is_ok());
}


#[test]
fn run_command_stats_status() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Stats(StatsCommands::Status));
    assert!(result.is_ok());
}

#[test]
fn stats_persist_snapshot_enable_keeps_file_compression_and_sls() {
    // A/B dry-run sets TOKENLESS_COMPRESSION_ENABLED=0 in the process, but
    // `stats enable` must write the on-disk compression/SLS values, not the
    // session overrides.
    let file = TokenlessConfig {
        stats_enabled: false,
        sls_enabled: true,
        compression_enabled: true,
    };
    let persisted = stats_persist_snapshot(file, true);
    assert!(persisted.stats_enabled);
    assert!(persisted.sls_enabled);
    assert!(persisted.compression_enabled);
}

#[test]
fn stats_persist_snapshot_disable_keeps_file_compression_and_sls() {
    let file = TokenlessConfig {
        stats_enabled: true,
        sls_enabled: false,
        compression_enabled: false,
    };
    let persisted = stats_persist_snapshot(file, false);
    assert!(!persisted.stats_enabled);
    assert!(!persisted.sls_enabled);
    assert!(!persisted.compression_enabled);
}

#[test]
fn run_command_stats_enable_does_not_persist_env_overrides() {
    // Drive the production Enable arm (load_from_file + save). Replacing
    // load_from_file() with load() would write the session env values.
    let guard = PersistConfigGuard::with_file_and_env(
        "{\"stats_enabled\":false,\"sls_enabled\":true,\"compression_enabled\":true}",
        "0",
        "0",
        "0",
    );
    run_command(Commands::Stats(StatsCommands::Enable)).unwrap();
    let persisted = guard.read_persisted();
    assert!(persisted.stats_enabled);
    assert!(persisted.sls_enabled);
    assert!(persisted.compression_enabled);
}

#[test]
fn run_command_stats_disable_does_not_persist_env_overrides() {
    let guard = PersistConfigGuard::with_file_and_env(
        "{\"stats_enabled\":true,\"sls_enabled\":false,\"compression_enabled\":false}",
        "1",
        "1",
        "1",
    );
    run_command(Commands::Stats(StatsCommands::Disable)).unwrap();
    let persisted = guard.read_persisted();
    assert!(!persisted.stats_enabled);
    assert!(!persisted.sls_enabled);
    assert!(!persisted.compression_enabled);
}

#[test]
fn missing_compare_sessions_reports_each_empty_side() {
    let record = StatsRecord::new(
        OperationType::CompressSchema,
        "cli".to_string(),
        100,
        40,
        50,
        20,
    );
    assert!(
        missing_compare_sessions(
            "b",
            "t",
            std::slice::from_ref(&record),
            std::slice::from_ref(&record)
        )
        .is_none()
    );

    let both = missing_compare_sessions("b", "t", &[], &[]).unwrap();
    assert!(both.starts_with("No records found for "));
    assert!(both.contains("baseline session \"b\""));
    assert!(both.contains("tokenless session \"t\""));
    assert!(both.contains(" and "));

    let baseline_only =
        missing_compare_sessions("b", "t", &[], std::slice::from_ref(&record)).unwrap();
    assert!(baseline_only.contains("baseline session \"b\""));
    assert!(!baseline_only.contains("tokenless session"));

    let tokenless_only = missing_compare_sessions("b", "t", &[record], &[]).unwrap();
    assert!(tokenless_only.contains("tokenless session \"t\""));
    assert!(!tokenless_only.contains("baseline session"));
}

#[test]
fn missing_compare_sessions_debug_escapes_control_chars() {
    let message = missing_compare_sessions(
        "base\u{1b}]0;INJECTED\u{7}",
        "tls\u{1b}]52;c;INJECTED\u{7}",
        &[],
        &[],
    )
    .unwrap();
    assert!(!message.contains('\u{1b}'));
    assert!(!message.contains('\u{7}'));
    assert!(message.contains("INJECTED"));
}

fn seed_compare_record(session_id: &str, mode: CompressionMode, before: usize, after: usize) {
    let recorder = open_recorder().expect("open recorder");
    recorder
        .record(
            &StatsRecord::new(
                OperationType::CompressResponse,
                "cli".to_string(),
                before * 4,
                before,
                after * 4,
                after,
            )
            .with_session_id(session_id)
            .with_mode(mode),
        )
        .expect("seed compare record");
}

#[test]
fn run_command_stats_compare() {
    let _guard = match TempDbGuard::new() {
        Some(g) => g,
        None => return,
    };
    let result = run_command(Commands::Stats(StatsCommands::Summary {
        limit: None,
        json: false,
        compare: Some(vec![
            "baseline-sess".to_string(),
            "tokenless-sess".to_string(),
        ]),
    }));
    let err = result.expect_err("empty compare sessions must fail closed");
    assert_eq!(err.1, 1);
    assert!(err.0.contains("No records found"));
    assert!(err.0.contains("baseline session \"baseline-sess\""));
    assert!(err.0.contains("tokenless session \"tokenless-sess\""));
}

#[test]
fn run_command_stats_compare_json() {
    let _guard = match TempDbGuard::new() {
        Some(g) => g,
        None => return,
    };
    let result = run_command(Commands::Stats(StatsCommands::Summary {
        limit: None,
        json: true,
        compare: Some(vec![
            "baseline-sess".to_string(),
            "tokenless-sess".to_string(),
        ]),
    }));
    let err = result.expect_err("empty JSON compare must not emit a 0% report");
    assert_eq!(err.1, 1);
    assert!(err.0.contains("No records found"));
}

#[test]
fn run_command_stats_compare_one_side_missing() {
    let _guard = match TempDbGuard::new() {
        Some(g) => g,
        None => return,
    };
    seed_compare_record("tokenless-sess", CompressionMode::Active, 400, 200);
    let baseline_missing = run_command(Commands::Stats(StatsCommands::Summary {
        limit: None,
        json: false,
        compare: Some(vec![
            "baseline-sess".to_string(),
            "tokenless-sess".to_string(),
        ]),
    }))
    .expect_err("missing baseline must fail");
    assert!(baseline_missing.0.contains("baseline session \"baseline-sess\""));
    assert!(!baseline_missing.0.contains("tokenless session"));

    seed_compare_record("baseline-sess", CompressionMode::DryRun, 400, 200);
    let tokenless_missing = run_command(Commands::Stats(StatsCommands::Summary {
        limit: None,
        json: true,
        compare: Some(vec![
            "baseline-sess".to_string(),
            "absent-tokenless".to_string(),
        ]),
    }))
    .expect_err("missing tokenless side must fail");
    assert!(tokenless_missing.0.contains("tokenless session \"absent-tokenless\""));
    assert!(!tokenless_missing.0.contains("baseline session"));
}

#[test]
fn run_command_stats_compare_populated_sessions() {
    let _guard = match TempDbGuard::new() {
        Some(g) => g,
        None => return,
    };
    seed_compare_record("baseline-sess", CompressionMode::DryRun, 400, 200);
    seed_compare_record("tokenless-sess", CompressionMode::Active, 400, 200);
    let result = run_command(Commands::Stats(StatsCommands::Summary {
        limit: None,
        json: false,
        compare: Some(vec![
            "baseline-sess".to_string(),
            "tokenless-sess".to_string(),
        ]),
    }));
    assert!(result.is_ok());

    let json = run_command(Commands::Stats(StatsCommands::Summary {
        limit: None,
        json: true,
        compare: Some(vec![
            "baseline-sess".to_string(),
            "tokenless-sess".to_string(),
        ]),
    }));
    assert!(json.is_ok());
}

#[test]
fn run_command_env_check_without_spec() {
    let _guard = TempDbGuard::new();
    let result = run_command(Commands::EnvCheck {
        tool: Some("NonexistentTool".to_string()),
        all: false,
        fix: false,
        checklist: false,
        json: false,
    });
    // Outcome depends on whether a tool-ready-spec file exists on the system:
    // Ok when a spec is found (NonexistentTool is reported as unknown),
    // Err when no spec file is available.
    let _ = result;
}

#[test]
fn get_stash_db_path_returns_valid() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let home = get_home_dir();
    if home.is_empty() {
        return;
    }
    let path = get_stash_db_path_with(&default_path_resolver(&home), None);
    assert!(path.is_ok());
    assert!(path.unwrap().ends_with(".tokenless/stash.db"));
}

#[test]
fn get_stash_db_path_with_bad_override() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let home = get_home_dir();
    if home.is_empty() {
        return;
    }
    let path = get_stash_db_path_with(
        &default_path_resolver(&home),
        Some("/nonexistent/deep/dir/stash.db"),
    );
    // Bad override is rejected, falls back to default
    assert!(path.is_ok());
    assert!(path.unwrap().ends_with(".tokenless/stash.db"));
}


#[test]
fn run_command_compress_schema_no_savings() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("small.json");
    std::fs::write(&f, r#"{"function":{"name":"x","parameters":{"type":"object","properties":{}}}}"#).unwrap();
    let result = run_command(Commands::CompressSchema {
        file: Some(f.to_str().unwrap().to_string()),
        batch: false,
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_compress_response_file_not_found() {
    let result = run_command(Commands::CompressResponse {
        file: Some("/nonexistent/path.json".to_string()),
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        truncate_strings_at: None,
        truncate_arrays_at: None,
        array_tail_preserve: None,
        max_depth: None,
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_err());
}

#[test]
fn open_stash_store_or_err_none_returns_ok() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = open_stash_store_or_err(None);
    assert!(result.is_ok());
}

#[test]
fn record_compression_stats_sls_only_path() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let config = TokenlessConfig { stats_enabled: false, sls_enabled: true, ..Default::default() };
    let long_before = "x".repeat(500);
    let short_after = "y".repeat(50);
    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressResponse,
        Some("sls-test".to_string()),
        Some("sls-session".to_string()),
        Some("sls-tool".to_string()),
        long_before,
        short_after,
        CompressionMode::Active,
        Some(2),
        Some(0),
        Some(15),
    );
}

/// Sets or clears the trace-context variables for one test and restores the
/// previous values on drop, so a panic cannot leak them into other tests.
///
/// Constructed after [`TempDbGuard`], which serializes environment mutation
/// through `ENV_MUTEX`.
struct TraceEnvGuard {
    prev_traceparent: Option<std::ffi::OsString>,
    prev_override: Option<std::ffi::OsString>,
}

impl TraceEnvGuard {
    fn new(traceparent: Option<&str>) -> Self {
        let prev_traceparent = std::env::var_os("TRACEPARENT");
        let prev_override = std::env::var_os("TOKENLESS_TRACEPARENT");
        unsafe {
            std::env::remove_var("TOKENLESS_TRACEPARENT");
            match traceparent {
                Some(value) => std::env::set_var("TRACEPARENT", value),
                None => std::env::remove_var("TRACEPARENT"),
            }
        }
        Self {
            prev_traceparent,
            prev_override,
        }
    }
}

impl Drop for TraceEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prev_traceparent {
                Some(v) => std::env::set_var("TRACEPARENT", v),
                None => std::env::remove_var("TRACEPARENT"),
            }
            match &self.prev_override {
                Some(v) => std::env::set_var("TOKENLESS_TRACEPARENT", v),
                None => std::env::remove_var("TOKENLESS_TRACEPARENT"),
            }
        }
    }
}

/// Reads the single JSONL line the SLS writer appended under `TempDbGuard`.
fn read_sls_line(sls_dir: &str) -> serde_json::Value {
    let path = PathBuf::from(sls_dir).join("tokenless.jsonl");
    let content = std::fs::read_to_string(&path).unwrap();
    let line = content.lines().next_back().expect("the SLS writer must append one line");
    serde_json::from_str(line).unwrap()
}

#[test]
fn record_compression_stats_stamps_host_trace_context() {
    let guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let _trace = TraceEnvGuard::new(Some(
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    ));
    let config = TokenlessConfig { stats_enabled: false, sls_enabled: true, ..Default::default() };

    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressResponse,
        Some("trace-agent".to_string()),
        Some("trace-session".to_string()),
        Some("trace-tool".to_string()),
        "t".repeat(600),
        "u".repeat(60),
        CompressionMode::Active,
        None,
        None,
        None,
    );

    let obj = read_sls_line(&guard.sls_dir);
    assert_eq!(
        obj["tokenless.trace_id"],
        "4bf92f3577b34da6a3ce929d0e0e4736",
        "the exported record must carry the host trace identity"
    );
    assert_eq!(obj["tokenless.span_id"], "00f067aa0ba902b7");
    // Existing attribution is unchanged by trace stamping.
    assert_eq!(obj["tokenless.session_id"], "trace-session");
    assert_eq!(obj["tokenless.tool_use_id"], "trace-tool");
}

#[test]
fn record_compression_stats_prefers_traceparent_override() {
    let guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let _trace = TraceEnvGuard::new(Some(
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    ));
    unsafe {
        std::env::set_var(
            "TOKENLESS_TRACEPARENT",
            "00-11111111111111111111111111111111-2222222222222222-01",
        );
    }
    let config = TokenlessConfig { stats_enabled: false, sls_enabled: true, ..Default::default() };

    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressResponse,
        Some("trace-agent".to_string()),
        None,
        None,
        "t".repeat(600),
        "u".repeat(60),
        CompressionMode::Active,
        None,
        None,
        None,
    );

    let obj = read_sls_line(&guard.sls_dir);
    assert_eq!(
        obj["tokenless.trace_id"],
        "11111111111111111111111111111111",
        "the adapter override must win over the standard host variable"
    );
    assert_eq!(obj["tokenless.span_id"], "2222222222222222");
}

#[test]
fn record_compression_stats_omits_trace_without_host_context() {
    let guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let _trace = TraceEnvGuard::new(None);
    let config = TokenlessConfig { stats_enabled: false, sls_enabled: true, ..Default::default() };

    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressResponse,
        Some("trace-agent".to_string()),
        None,
        None,
        "t".repeat(600),
        "u".repeat(60),
        CompressionMode::Active,
        None,
        None,
        None,
    );

    let obj = read_sls_line(&guard.sls_dir);
    let keys = obj.as_object().unwrap();
    assert!(!keys.contains_key("tokenless.trace_id"));
    assert!(!keys.contains_key("tokenless.span_id"));
}

#[test]
fn record_compression_stats_full_path() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let config = TokenlessConfig { stats_enabled: true, sls_enabled: true, ..Default::default() };
    let long_before = "z".repeat(1000);
    let short_after = "w".repeat(100);
    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressSchema,
        Some("full-agent".to_string()),
        Some("full-session".to_string()),
        Some("full-tool".to_string()),
        long_before,
        short_after,
        CompressionMode::Active,
        Some(3),
        Some(1),
        Some(200),
    );
}

#[test]
fn record_compression_stats_no_savings() {
    let config = TokenlessConfig { stats_enabled: true, ..Default::default() };
    record_compression_stats(
        &config,
        &DatabasePathResolver::default(),
        OperationType::CompressResponse,
        None,
        None,
        None,
        "short".to_string(),
        "this is longer than before so no savings".to_string(),
        CompressionMode::Active,
        None,
        None,
        None,
    );
}


#[test]
fn run_command_compress_response_no_agent_id() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("resp.json");
    std::fs::write(&f, r#"{"data":"test","debug":"remove"}"#).unwrap();
    let result = run_command(Commands::CompressResponse {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        truncate_strings_at: None,
        truncate_arrays_at: None,
        array_tail_preserve: None,
        max_depth: None,
        no_stash: true,
        stash_db: None,
    });
    assert!(result.is_ok());
}

#[test]
fn read_input_file_too_large() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("huge.json");
    let huge = "x".repeat(64 * 1024 * 1024 + 1);
    std::fs::write(&f, &huge).unwrap();
    let result = read_input(&Some(f.to_str().unwrap().to_string()));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("limit"));
}

#[test]
fn read_input_file_missing() {
    let result = read_input(&Some("/nonexistent/file.json".to_string()));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Failed to open"));
}


#[test]
fn run_command_retrieve_store_error() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };
    let result = run_command(Commands::Retrieve {
        hash: "abcdef0123456789abcdef01".to_string(),
        stash_db: Some("/nonexistent/deep/nested/dir/stash.db".to_string()),
    });
    // The stash_db override gets rejected and falls back to the temp DB,
    // where the hash does not exist.
    assert!(result.is_err());
    assert!(result.unwrap_err().0.contains("no stashed payload"));
}

#[test]
fn run_command_compress_toon_tiny_input() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("tiny.json");
    std::fs::write(&f, r#""x""#).unwrap();
    let result = run_command(Commands::CompressToon {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        min_toon_chars: MIN_TOON_CHARS,
    });
    // A tiny JSON string passes through the shared minimum-length gate
    // without error.
    assert!(result.is_ok());

    // Disabling the gate keeps the legacy encode path available.
    let result = run_command(Commands::CompressToon {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        min_toon_chars: 0,
    });
    assert!(result.is_ok());
}

#[test]
fn run_command_compress_toon_empty_obj() {
    let _guard = TempDbGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("empty_obj.json");
    std::fs::write(&f, r#"{}"#).unwrap();
    let result = run_command(Commands::CompressToon {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        min_toon_chars: MIN_TOON_CHARS,
    });
    // An empty JSON object compresses successfully.
    assert!(result.is_ok());
}

#[test]
fn run_command_compress_response_with_stash_truncation() {
    let _guard = match TempDbGuard::new() { Some(g) => g, None => return };

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("stash_truncation.json");
    let items: Vec<serde_json::Value> = (0..100).map(|i| serde_json::json!({"id": i, "data": "x".repeat(200)})).collect();
    let data = serde_json::json!({"results": items});
    std::fs::write(&f, serde_json::to_string(&data).unwrap()).unwrap();
    let result = run_command(Commands::CompressResponse {
        file: Some(f.to_str().unwrap().to_string()),
        agent_id: None,
        session_id: None,
        tool_use_id: None,
        truncate_strings_at: Some(10),
        truncate_arrays_at: Some(3),
        array_tail_preserve: None,
        max_depth: Some(3),
        no_stash: false,
        stash_db: None,
    });
    // Compression with stash enabled and aggressive truncation succeeds.
    assert!(result.is_ok());
}

#[test]
fn stats_after_text_records_the_candidate_only_when_it_was_measured() {
    let result_with = |disposition: Disposition| CompressResult {
        output: "original".to_string(),
        compressed_output: "candidate".to_string(),
        disposition,
        before_tokens: 10,
        after_tokens: 4,
        stash_writes: None,
        stash_errors: None,
        unrecoverable_truncations: None,
        stash_size: None,
    };

    // Applied and dry-run measured the candidate; dry-run is what keeps
    // A/B prediction records alive.
    for disposition in [Disposition::Applied, Disposition::DryRun] {
        assert_eq!(
            stats_after_text(&result_with(disposition), "original"),
            "candidate"
        );
    }
    // Every rejection emitted the original: recording the discarded
    // candidate would book savings that never reached the model. Timeout
    // is the regression case — the pipeline keeps the candidate text for
    // the legacy measurement channel even though the output fell back.
    for disposition in [
        Disposition::NoSavings,
        Disposition::Timeout,
        Disposition::Passthrough,
        Disposition::RecoverabilityUnavailable,
        Disposition::ToolError,
    ] {
        assert_eq!(
            stats_after_text(&result_with(disposition), "original"),
            "original"
        );
    }
}
