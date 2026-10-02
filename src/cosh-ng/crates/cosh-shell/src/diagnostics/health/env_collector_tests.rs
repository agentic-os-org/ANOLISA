use super::*;

/// Single process-wide lock for every test that mutates HOME or credential
/// env vars, shared with env-reading tests in other modules via
/// `crate::diagnostics::test_env`; using separate mutexes lets tests race on
/// the same global env and read each other's config.toml.
use crate::diagnostics::test_env::env_guard;

#[test]
fn classify_provider_covers_known_and_unknown_adapters() {
    assert_eq!(classify_provider("fake", false), ProviderReadiness::Ready);
    assert_eq!(
        classify_provider("cosh-core", true),
        ProviderReadiness::Ready
    );
    assert_eq!(
        classify_provider("cosh-core", false),
        ProviderReadiness::MissingCredentials
    );
    assert_eq!(classify_provider("qwen", true), ProviderReadiness::Ready);
    assert_eq!(
        classify_provider("", false),
        ProviderReadiness::UnknownAdapter
    );
    assert_eq!(
        classify_provider("mystery", false),
        ProviderReadiness::UnknownAdapter
    );
    // Unknown adapters are never ready, even with generic credentials
    // present: the adapter registry rejects the name first.
    assert_eq!(
        classify_provider("mystery", true),
        ProviderReadiness::UnknownAdapter
    );
}

#[test]
fn classify_hooks_flags_untrusted_project_only() {
    assert_eq!(classify_hooks(false, true), HooksReadiness::Ok);
    assert_eq!(classify_hooks(true, true), HooksReadiness::Ok);
    assert_eq!(
        classify_hooks(true, false),
        HooksReadiness::ProjectUntrusted
    );
    // No project hooks present -> trusted flag is irrelevant.
    assert_eq!(classify_hooks(false, false), HooksReadiness::Ok);
}

#[test]
fn classify_permissions_flags_unwritable() {
    assert_eq!(classify_permissions(true), PermissionsReadiness::Ok);
    assert_eq!(
        classify_permissions(false),
        PermissionsReadiness::Unwritable
    );
}

#[test]
fn collectors_record_checks_without_panicking() {
    let config = CoshConfig::default();
    let mut builder = HealthReportBuilder::for_started_at(0);
    run_env_collectors(&mut builder, &config, Path::new("/tmp"), 0);
    let report = builder.finish(1);
    for check in ["provider", "config", "hooks", "pty", "permissions"] {
        assert!(
            report.checks_done.iter().any(|done| done == check),
            "missing check {check}: {report:?}"
        );
    }
}

#[test]
fn config_status_flags_invalid_toml_as_unparseable() {
    // Serialize HOME mutation so parallel tests do not clobber each other.
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-config-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let previous_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &dir);

    // Valid TOML -> consumable.
    std::fs::write(
        config_dir.join("config.toml"),
        "[ui]\nlanguage = \"en-US\"\n",
    )
    .expect("write valid config");
    let ok_status = config_file_status();
    assert!(ok_status.readable && ok_status.parseable, "{ok_status:?}");

    // Readable but invalid TOML -> parseable=false, so a finding is emitted.
    std::fs::write(config_dir.join("config.toml"), "this = = not valid toml\n")
        .expect("write invalid config");
    let bad_status = config_file_status();
    assert!(bad_status.readable, "{bad_status:?}");
    assert!(!bad_status.parseable, "{bad_status:?}");

    let mut builder = HealthReportBuilder::for_started_at(0);
    collect_config(&mut builder, &CoshConfig::default(), 0);
    let report = builder.finish(1);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.id == "env-config"),
        "invalid TOML must emit an env-config finding: {report:?}"
    );

    match previous_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cosh_core_requires_both_access_key_id_and_secret() {
    let _guard = env_guard();

    // Isolate HOME to a temp dir with an aliyun provider config so the
    // env check uses the aliyun branch (AK/SK only).
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-provider-env-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).expect("create isolated home");
    let prev_home = std::env::var_os("HOME");
    let prev_id = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_ID");
    let prev_secret = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    let prev_dash = std::env::var_os("DASHSCOPE_API_KEY");
    let prev_openai = std::env::var_os("OPENAI_API_KEY");
    std::env::set_var("HOME", &dir);
    std::env::remove_var("DASHSCOPE_API_KEY");
    std::env::remove_var("OPENAI_API_KEY");

    // Set provider_type = "aliyun" so env check resolves the aliyun branch.
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"default\"\n\n[ai.providers.default]\ntype = \"aliyun\"\n",
    )
    .expect("write aliyun provider type");

    std::env::set_var("ALIBABA_CLOUD_ACCESS_KEY_ID", "id-only");
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    assert!(
        !provider_credentials_present("cosh-core"),
        "access key id alone must not be treated as ready"
    );

    std::env::set_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET", "secret");
    assert!(
        provider_credentials_present("cosh-core"),
        "AK id + secret must be treated as ready for aliyun provider type"
    );

    restore_env("ALIBABA_CLOUD_ACCESS_KEY_ID", prev_id);
    restore_env("ALIBABA_CLOUD_ACCESS_KEY_SECRET", prev_secret);
    restore_env("DASHSCOPE_API_KEY", prev_dash);
    restore_env("OPENAI_API_KEY", prev_openai);
    restore_env("HOME", prev_home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cosh_core_api_key_env_satisfies_non_aliyun_provider() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-apikey-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&dir).expect("create isolated home");
    let prev_home = std::env::var_os("HOME");
    let prev_id = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_ID");
    let prev_secret = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    let prev_dash = std::env::var_os("DASHSCOPE_API_KEY");
    let prev_openai = std::env::var_os("OPENAI_API_KEY");
    std::env::set_var("HOME", &dir);
    // No AK/SK, no config — only API key env vars.
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_ID");
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");

    std::env::set_var("DASHSCOPE_API_KEY", "sk-test");
    assert!(
        provider_credentials_present("cosh-core"),
        "DASHSCOPE_API_KEY must satisfy cosh-core for non-aliyun provider types"
    );

    std::env::remove_var("DASHSCOPE_API_KEY");
    std::env::set_var("OPENAI_API_KEY", "sk-openai");
    assert!(
        provider_credentials_present("cosh-core"),
        "OPENAI_API_KEY must satisfy cosh-core for generic provider types"
    );

    restore_env("ALIBABA_CLOUD_ACCESS_KEY_ID", prev_id);
    restore_env("ALIBABA_CLOUD_ACCESS_KEY_SECRET", prev_secret);
    restore_env("DASHSCOPE_API_KEY", prev_dash);
    restore_env("OPENAI_API_KEY", prev_openai);
    restore_env("HOME", prev_home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn aliyun_provider_type_ignores_api_key_in_config() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-aliyun-nokey-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let prev_home = std::env::var_os("HOME");
    let prev_id = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_ID");
    let prev_secret = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    let prev_dash = std::env::var_os("DASHSCOPE_API_KEY");
    let prev_openai = std::env::var_os("OPENAI_API_KEY");
    std::env::set_var("HOME", &dir);
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_ID");
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    std::env::remove_var("DASHSCOPE_API_KEY");
    std::env::remove_var("OPENAI_API_KEY");

    // provider_type = "aliyun" with only api_key — cosh-core ignores
    // api_key for aliyun and falls back to mock, so the doctor must not
    // report readiness.
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"default\"\n\n[ai.providers.default]\ntype = \"aliyun\"\napi_key = \"sk-test\"\n",
    )
    .expect("write aliyun with only api_key");
    assert!(
        !provider_credentials_present("cosh-core"),
        "aliyun provider_type with only api_key must not satisfy readiness"
    );

    // Generic provider_type with api_key IS ready.
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"default\"\n\n[ai.providers.default]\ntype = \"dashscope\"\napi_key = \"sk-test\"\n",
    )
    .expect("write generic with api_key");
    assert!(
        provider_credentials_present("cosh-core"),
        "generic provider_type with api_key must satisfy readiness"
    );

    restore_env("ALIBABA_CLOUD_ACCESS_KEY_ID", prev_id);
    restore_env("ALIBABA_CLOUD_ACCESS_KEY_SECRET", prev_secret);
    restore_env("DASHSCOPE_API_KEY", prev_dash);
    restore_env("OPENAI_API_KEY", prev_openai);
    restore_env("HOME", prev_home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_adapter_is_never_ready_even_with_generic_key() {
    let _guard = env_guard();

    let prev_key = std::env::var_os("OPENAI_API_KEY");
    std::env::set_var("OPENAI_API_KEY", "sk-fake");
    assert!(
        !provider_credentials_present("not-a-real-adapter"),
        "an unregistered adapter must not be satisfied by a generic key"
    );
    assert_eq!(
        classify_provider("not-a-real-adapter", true),
        ProviderReadiness::UnknownAdapter
    );
    restore_env("OPENAI_API_KEY", prev_key);
}

#[test]
fn cosh_core_credentials_from_config_toml_are_recognized() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-provider-cfg-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let prev_home = std::env::var_os("HOME");
    let prev_id = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_ID");
    let prev_secret = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    let prev_dash = std::env::var_os("DASHSCOPE_API_KEY");
    let prev_openai = std::env::var_os("OPENAI_API_KEY");
    std::env::set_var("HOME", &dir);
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_ID");
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    std::env::remove_var("DASHSCOPE_API_KEY");
    std::env::remove_var("OPENAI_API_KEY");

    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"aliyun\"\n\n[ai.providers.aliyun]\ntype = \"aliyun\"\naccess_key_id = \"manual-ak\"\naccess_key_secret = \"manual-sk\"\n",
    )
    .expect("write provider config");
    assert!(
        provider_credentials_present("cosh-core"),
        "config-backed AK/SK on the active aliyun provider must satisfy cosh-core readiness"
    );

    // ECS RAM role auth source is also accepted without AK/SK.
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"aliyun\"\n\n[ai.providers.aliyun]\ntype = \"aliyun\"\nauth_source = \"ecs_ram_role\"\n",
    )
    .expect("write ecs config");
    assert!(
        provider_credentials_present("cosh-core"),
        "ecs_ram_role auth source on the active aliyun provider must satisfy cosh-core readiness"
    );

    restore_env("ALIBABA_CLOUD_ACCESS_KEY_ID", prev_id);
    restore_env("ALIBABA_CLOUD_ACCESS_KEY_SECRET", prev_secret);
    restore_env("DASHSCOPE_API_KEY", prev_dash);
    restore_env("OPENAI_API_KEY", prev_openai);
    restore_env("HOME", prev_home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_active_provider_credentials_are_ignored() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-non-active-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let prev_home = std::env::var_os("HOME");
    let prev_id = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_ID");
    let prev_secret = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    let prev_dash = std::env::var_os("DASHSCOPE_API_KEY");
    let prev_openai = std::env::var_os("OPENAI_API_KEY");
    std::env::set_var("HOME", &dir);
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_ID");
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    std::env::remove_var("DASHSCOPE_API_KEY");
    std::env::remove_var("OPENAI_API_KEY");

    // active_provider = "default" but credentials are under "aliyun".
    // cosh-core only reads the active provider, so this must not satisfy
    // readiness — matching the real Core behavior which falls back to mock
    // when the active provider lacks credentials.
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"default\"\n\n[ai.providers.aliyun]\naccess_key_id = \"ak\"\naccess_key_secret = \"sk\"\n",
    )
    .expect("write mismatched provider config");
    assert!(
        !provider_credentials_present("cosh-core"),
        "credentials on a non-active provider must not satisfy readiness"
    );

    restore_env("ALIBABA_CLOUD_ACCESS_KEY_ID", prev_id);
    restore_env("ALIBABA_CLOUD_ACCESS_KEY_SECRET", prev_secret);
    restore_env("DASHSCOPE_API_KEY", prev_dash);
    restore_env("OPENAI_API_KEY", prev_openai);
    restore_env("HOME", prev_home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn aliyun_provider_type_env_openai_key_is_not_ready() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-aliyun-env-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let prev_home = std::env::var_os("HOME");
    let prev_id = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_ID");
    let prev_secret = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    let prev_dash = std::env::var_os("DASHSCOPE_API_KEY");
    let prev_openai = std::env::var_os("OPENAI_API_KEY");
    std::env::set_var("HOME", &dir);
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_ID");
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    std::env::remove_var("DASHSCOPE_API_KEY");

    // provider_type = "aliyun" with only OPENAI_API_KEY in env — cosh-core
    // ignores generic API-key env vars for aliyun, so doctor must not
    // report ready.
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"default\"\n\n[ai.providers.default]\ntype = \"aliyun\"\n",
    )
    .expect("write aliyun provider type");
    std::env::set_var("OPENAI_API_KEY", "sk-fake");
    assert!(
        !provider_credentials_present("cosh-core"),
        "aliyun provider_type must ignore OPENAI_API_KEY env var"
    );

    restore_env("ALIBABA_CLOUD_ACCESS_KEY_ID", prev_id);
    restore_env("ALIBABA_CLOUD_ACCESS_KEY_SECRET", prev_secret);
    restore_env("DASHSCOPE_API_KEY", prev_dash);
    restore_env("OPENAI_API_KEY", prev_openai);
    restore_env("HOME", prev_home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cosh_ai_provider_env_overrides_active_provider() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-override-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let prev_home = std::env::var_os("HOME");
    let prev_id = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_ID");
    let prev_secret = std::env::var_os("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
    let prev_override = std::env::var_os("COSH_AI_PROVIDER");
    std::env::set_var("HOME", &dir);
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_ID");
    std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");

    // Config: default provider is aliyun (no creds), but COSH_AI_PROVIDER
    // overrides to a generic provider with api_key. Doctor should follow
    // the override, not the config default.
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"default\"\n\n[ai.providers.default]\ntype = \"aliyun\"\n\n[ai.providers.generic]\ntype = \"openai_compat\"\napi_key = \"sk-test\"\n",
    )
    .expect("write override config");
    std::env::set_var("COSH_AI_PROVIDER", "generic");
    assert!(
        provider_credentials_present("cosh-core"),
        "COSH_AI_PROVIDER override to generic provider with api_key must satisfy readiness"
    );

    // Override to aliyun provider with no AK/SK — must not be ready even
    // if the config default has other credentials.
    std::env::set_var("COSH_AI_PROVIDER", "default");
    assert!(
        !provider_credentials_present("cosh-core"),
        "COSH_AI_PROVIDER override to aliyun without AK/SK must not satisfy readiness"
    );

    restore_env("ALIBABA_CLOUD_ACCESS_KEY_ID", prev_id);
    restore_env("ALIBABA_CLOUD_ACCESS_KEY_SECRET", prev_secret);
    restore_env("COSH_AI_PROVIDER", prev_override);
    restore_env("HOME", prev_home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_simple_config_is_not_flagged() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-legacy-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let prev_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &dir);

    // Legacy `key = value` with an unquoted value is not valid TOML but is
    // consumed by parse_simple_config, so the doctor must not flag it.
    std::fs::write(config_dir.join("config.toml"), "ui.language = zh-CN\n")
        .expect("write legacy config");
    let status = config_file_status();
    assert!(
        status.readable && status.parseable,
        "legacy simple config must be treated as consumable: {status:?}"
    );

    restore_env("HOME", prev_home);
    let _ = std::fs::remove_dir_all(&dir);
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

fn restore_env(key: &str, previous: Option<std::ffi::OsString>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}
