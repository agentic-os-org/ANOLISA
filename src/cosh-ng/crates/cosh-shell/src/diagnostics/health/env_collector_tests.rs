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
    for check in [
        "provider",
        "config",
        "hooks",
        "pty",
        "permissions",
        "runtime",
        "logs",
    ] {
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
fn runtime_collector_reports_crash_stale_and_orphan_findings() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-runtime-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let state_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(state_dir.join("run")).expect("create run dir");
    let previous_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &dir);

    let dead_pid = 999_999_u32;
    let live_pid = std::process::id();
    let now = env_now_ms();

    // A fresh crash record within the 24h window -> critical finding.
    let crash_line = format!(
        "{{\"ts\":{now},\"version\":\"test\",\"pid\":{dead_pid},\"panic\":\"boom\",\"location\":\"t.rs:1\"}}\n"
    );
    std::fs::write(state_dir.join("cosh-shell-crash.log"), &crash_line).expect("write crash log");

    // An orphan core: alive (this process), owner dead, headless mode.
    let orphan = crate::diagnostics::run_registry::RunEntry {
        kind: "core".to_string(),
        pid: live_pid,
        ppid: None,
        version: "test".to_string(),
        start_ts_ms: now,
        shell_kind: None,
        mode: Some("headless".to_string()),
        session_id: Some("s-1".to_string()),
        core_pid: None,
        owner_shell_pid: Some(dead_pid),
        routing: None,
    };
    std::fs::write(
        state_dir.join("run").join(format!("core-{live_pid}.json")),
        serde_json::to_string(&orphan).unwrap(),
    )
    .expect("write orphan core entry");

    // A stale shell entry: pid dead, no cleanup.
    let stale = crate::diagnostics::run_registry::RunEntry {
        kind: "shell".to_string(),
        pid: dead_pid,
        ppid: None,
        version: "test".to_string(),
        start_ts_ms: now,
        shell_kind: Some("zsh".to_string()),
        mode: None,
        session_id: Some("s-2".to_string()),
        core_pid: None,
        owner_shell_pid: None,
        routing: None,
    };
    std::fs::write(
        state_dir.join("run").join(format!("shell-{dead_pid}.json")),
        serde_json::to_string(&stale).unwrap(),
    )
    .expect("write stale shell entry");

    let mut builder = HealthReportBuilder::for_started_at(0);
    collect_runtime(&mut builder, 0);
    let report = builder.finish(1);

    let crash = report
        .findings
        .iter()
        .find(|finding| finding.id.starts_with("env-runtime-crash"))
        .expect("crash finding present");
    assert_eq!(crash.severity, HealthSeverity::Critical);
    assert_eq!(
        crash.detail_args.get("kind").map(String::as_str),
        Some("cosh-shell")
    );
    assert_eq!(
        crash.detail_args.get("panic").map(String::as_str),
        Some("boom")
    );

    let orphan = report
        .findings
        .iter()
        .find(|finding| finding.id == format!("env-runtime-orphan-{live_pid}"))
        .expect("orphan finding present");
    assert_eq!(orphan.severity, HealthSeverity::Warning);

    let stale = report
        .findings
        .iter()
        .find(|finding| finding.id == format!("env-runtime-stale-{dead_pid}"))
        .expect("stale finding present");
    assert_eq!(stale.severity, HealthSeverity::Warning);
    assert_eq!(
        stale.detail_args.get("kind").map(String::as_str),
        Some("cosh-shell")
    );
    assert_eq!(report.overall_severity, HealthSeverity::Critical);

    match previous_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn runtime_collector_ignores_out_of_window_crashes_and_brokered_cores() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-runtime-quiet-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let state_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(state_dir.join("run")).expect("create run dir");
    let previous_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &dir);

    let now = env_now_ms();
    let live_pid = std::process::id();

    // A crash older than 24h stays out of the doctor window.
    let old_ts = now - CRASH_WINDOW_MS - 60_000;
    let crash_line =
        format!("{{\"ts\":{old_ts},\"version\":\"test\",\"pid\":1,\"panic\":\"old\"}}\n");
    std::fs::write(state_dir.join("cosh-core-crash.log"), &crash_line).expect("write crash log");

    // A live brokered core with no owner shell is owned by an agent host,
    // not an orphan.
    let brokered = crate::diagnostics::run_registry::RunEntry {
        kind: "core".to_string(),
        pid: live_pid,
        ppid: None,
        version: "test".to_string(),
        start_ts_ms: now,
        shell_kind: None,
        mode: Some("brokered".to_string()),
        session_id: Some("s-3".to_string()),
        core_pid: None,
        owner_shell_pid: None,
        routing: None,
    };
    std::fs::write(
        state_dir.join("run").join(format!("core-{live_pid}.json")),
        serde_json::to_string(&brokered).unwrap(),
    )
    .expect("write brokered core entry");

    let mut builder = HealthReportBuilder::for_started_at(0);
    collect_runtime(&mut builder, 0);
    let report = builder.finish(1);

    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.id.starts_with("env-runtime-crash")),
        "old crashes must not surface: {report:?}"
    );
    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.id.starts_with("env-runtime-orphan")),
        "brokered cores are not orphans: {report:?}"
    );
    assert_eq!(report.overall_severity, HealthSeverity::Ok);

    match previous_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn logs_collector_counts_recent_errors_and_warns() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-logs-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let logs_dir = dir.join(".copilot-shell/logs");
    std::fs::create_dir_all(&logs_dir).expect("create logs dir");
    let previous_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &dir);

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let now_ms = env_now_ms();
    let header = |ts_ms: u128, level: &str, msg: &str| {
        let ts = chrono::DateTime::from_timestamp_millis(ts_ms as i64)
            .expect("valid timestamp")
            .to_rfc3339();
        format!("{ts} {level} cosh_core::adapter: {msg}\n")
    };

    // Two fresh ERROR lines plus one WARN; a continuation line and a
    // non-header line must not be counted.
    let mut core_log = String::new();
    core_log.push_str(&header(
        now_ms - 60_000,
        "ERROR",
        "turn failed token=sk-1234",
    ));
    core_log.push_str(&header(now_ms - 30_000, "ERROR", "turn failed again"));
    core_log.push_str(&header(now_ms - 10_000, "WARN", "retry scheduled"));
    core_log.push_str("  this is a continuation line\n");
    core_log.push_str("not a log header\n");
    std::fs::write(logs_dir.join(format!("cosh-core.log.{today}")), core_log)
        .expect("write core log");

    let shell_log = header(now_ms - 5_000, "WARN", "slow prompt");
    std::fs::write(logs_dir.join(format!("cosh-shell.log.{today}")), shell_log)
        .expect("write shell log");

    let mut builder = HealthReportBuilder::for_started_at(0);
    collect_logs(&mut builder, 0);
    let report = builder.finish(1);

    let fact = |key: &str| {
        report
            .facts
            .iter()
            .find(|fact| fact.key == key)
            .unwrap_or_else(|| panic!("missing fact {key}: {report:?}"))
    };
    assert_eq!(fact("logs.present").value, HealthFactValue::Bool(true));
    assert_eq!(fact("logs.warns_24h").value, HealthFactValue::Unsigned(2));
    assert_eq!(fact("logs.errors_24h").value, HealthFactValue::Unsigned(2));
    assert!(
        matches!(&fact("logs.error_sample").value, HealthFactValue::String(s) if s.contains("turn failed again")),
        "sample must be the latest error: {report:?}"
    );

    let errors = report
        .findings
        .iter()
        .find(|finding| finding.id == "env-logs-errors-cosh-core")
        .expect("core errors finding present");
    assert_eq!(errors.severity, HealthSeverity::Warning);
    assert_eq!(
        errors.detail_args.get("count").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        errors.detail_args.get("file").map(String::as_str),
        Some("cosh-core.log")
    );
    assert_eq!(errors.title_id, HealthMessageId::HealthFindingRecentErrors);
    assert_eq!(
        errors.detail_id,
        Some(HealthMessageId::HealthRemediationLogs)
    );
    assert!(
        errors
            .evidence_fact_ids
            .iter()
            .any(|id| id == "logs.error_sample"),
        "error finding must reference the sample evidence"
    );
    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.id.starts_with("env-logs-warn-flood")),
        "2 WARN lines are below the flood threshold: {report:?}"
    );
    assert_eq!(report.overall_severity, HealthSeverity::Warning);

    restore_env("HOME", previous_home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn logs_collector_ignores_old_lines_and_missing_files() {
    let _guard = env_guard();

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-logs-quiet-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let logs_dir = dir.join(".copilot-shell/logs");
    std::fs::create_dir_all(&logs_dir).expect("create logs dir");
    let previous_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &dir);

    // An ERROR older than the 24h window stays out of the counts.
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let old_ts = env_now_ms() - CRASH_WINDOW_MS - 60_000;
    let ts = chrono::DateTime::from_timestamp_millis(old_ts as i64)
        .expect("valid timestamp")
        .to_rfc3339();
    std::fs::write(
        logs_dir.join(format!("cosh-shell.log.{today}")),
        format!("{ts} ERROR cosh_core::adapter: old failure\n"),
    )
    .expect("write old log line");

    let mut builder = HealthReportBuilder::for_started_at(0);
    collect_logs(&mut builder, 0);
    let report = builder.finish(1);

    let fact = |key: &str| {
        report
            .facts
            .iter()
            .find(|fact| fact.key == key)
            .unwrap_or_else(|| panic!("missing fact {key}: {report:?}"))
    };
    assert_eq!(fact("logs.errors_24h").value, HealthFactValue::Unsigned(0));
    assert!(
        report
            .findings
            .iter()
            .all(|finding| !finding.id.starts_with("env-logs-errors")),
        "out-of-window errors must not surface: {report:?}"
    );
    assert_eq!(report.overall_severity, HealthSeverity::Ok);

    restore_env("HOME", previous_home.clone());
    let _ = std::fs::remove_dir_all(&dir);

    // A HOME without any log files yields no findings either.
    let empty_dir = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-logs-empty-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&empty_dir).expect("create empty home");
    std::env::set_var("HOME", &empty_dir);
    let mut builder = HealthReportBuilder::for_started_at(0);
    collect_logs(&mut builder, 0);
    let report = builder.finish(1);
    let fact = |key: &str| {
        report
            .facts
            .iter()
            .find(|fact| fact.key == key)
            .unwrap_or_else(|| panic!("missing fact {key}: {report:?}"))
    };
    assert_eq!(fact("logs.present").value, HealthFactValue::Bool(false));
    assert!(report.findings.is_empty(), "{report:?}");
    assert_eq!(report.overall_severity, HealthSeverity::Ok);

    restore_env("HOME", previous_home);
    let _ = std::fs::remove_dir_all(&empty_dir);
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
