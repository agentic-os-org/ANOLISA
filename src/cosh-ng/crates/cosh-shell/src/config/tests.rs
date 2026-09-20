use std::path::PathBuf;

use crate::types::CoshApprovalMode;

use super::hook_feedback::{
    hook_feedback_store_path_in_dir, read_hook_feedback_from_store_path,
    write_hook_feedback_entries_to_store_path, write_hook_feedback_to_store_path,
};
use super::language::{language_setting_from_config_content, write_language_config_to_path};
use super::load::{
    config_read_file_path_for_home, load_config_file_into,
    parse_recommendations_environment_override,
};
use super::parse::{parse_simple_config, parse_toml_config};
use super::trust::{
    add_trusted_project_root_to_store_path, load_project_trust_store,
    project_trust_store_path_in_dir, read_trusted_project_roots_from_store_path,
    remove_trusted_project_root_from_store_path, write_trusted_project_roots_to_store_path,
};
use super::{
    parse_language_setting, CoshConfig, HealthServiceExpectedState, HookFeedbackPreference,
    LanguageSetting,
};

fn temp_config_path(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "cosh-shell-config-{label}-{}-{nanos}",
            std::process::id()
        ))
        .join(".copilot-shell/config.toml")
}

fn temp_home_path(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "cosh-shell-home-{label}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn default_config_values() {
    let cfg = CoshConfig::default();
    assert_eq!(cfg.shell_default, "auto");
    assert_eq!(cfg.shell_integration, "enhanced");
    assert_eq!(cfg.analysis_mode, "smart");
    assert_eq!(cfg.approval_mode, CoshApprovalMode::Auto);
    assert_eq!(cfg.adapter_default, "cosh-core");
    assert_eq!(cfg.language, "auto");
    assert!(cfg.startup_banner);
    assert!(!cfg.startup_hooks);
    assert!(!cfg.debug);
    assert!(cfg.ai_enabled);
    assert!(cfg.health.enabled);
    assert_eq!(cfg.health.role, None);
    assert!(!cfg.health.memory_sensitive);
    assert_eq!(cfg.health.critical_mounts, vec!["/"]);
    assert!(!cfg.health.verbose);
    assert!(cfg.health.services.is_empty());
    assert!(cfg.trusted_project_roots.is_empty());
    assert!(cfg.readonly.disabled.is_empty());
    assert!(cfg.readonly.overrides.is_empty());
    assert!(cfg.recommendations.enabled);
    assert!(!cfg.recommendations.bash_history);
}

#[test]
fn recommendation_environment_override_is_separate_from_config_default() {
    assert_eq!(
        parse_recommendations_environment_override(Some("1")),
        Some(true)
    );
    assert_eq!(
        parse_recommendations_environment_override(Some("0")),
        Some(false)
    );
    assert_eq!(
        parse_recommendations_environment_override(Some("invalid")),
        None
    );
    assert_eq!(parse_recommendations_environment_override(None), None);
}

#[test]
fn parse_recommendation_config() {
    let mut cfg = CoshConfig::default();
    parse_toml_config(
        r#"
[shell.recommendations]
enabled = false
bash_history = true
"#,
        &mut cfg,
    );

    assert!(!cfg.recommendations.enabled);
    assert!(cfg.recommendations.bash_history);
}

#[test]
fn top_level_recommendations_table_does_not_extend_shared_config_schema() {
    let mut cfg = CoshConfig::default();
    parse_toml_config(
        "[recommendations]\nenabled = false\nbash_history = true\n",
        &mut cfg,
    );

    assert!(cfg.recommendations.enabled);
    assert!(!cfg.recommendations.bash_history);
}

#[test]
fn config_read_path_prefers_shared_copilot_shell_config() {
    let home = temp_home_path("shared-config-wins");
    let shared = home.join(".copilot-shell/config.toml");
    let legacy = home.join(".config/cosh/config.toml");
    std::fs::create_dir_all(shared.parent().unwrap()).expect("shared dir");
    std::fs::create_dir_all(legacy.parent().unwrap()).expect("legacy dir");
    std::fs::write(&shared, "[ui]\nlanguage = \"en-US\"\n").expect("shared config");
    std::fs::write(&legacy, "[ui]\nlanguage = \"zh-CN\"\n").expect("legacy config");

    assert_eq!(config_read_file_path_for_home(&home), shared);

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn config_read_path_ignores_legacy_cosh_config() {
    let home = temp_home_path("legacy-config-ignored");
    let legacy = home.join(".config/cosh/config.toml");
    std::fs::create_dir_all(legacy.parent().unwrap()).expect("legacy dir");
    std::fs::write(&legacy, "[ui]\nlanguage = \"zh-CN\"\n").expect("legacy config");

    assert_eq!(
        config_read_file_path_for_home(&home),
        home.join(".copilot-shell/config.toml")
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn cosh_state_paths_use_copilot_shell_cosh_dir() {
    let home = temp_home_path("cosh-state-paths");
    let cosh_dir = home.join(".copilot-shell/cosh");

    assert_eq!(
        project_trust_store_path_in_dir(&cosh_dir),
        home.join(".copilot-shell/cosh/trusted-project-hooks")
    );
    assert_eq!(
        hook_feedback_store_path_in_dir(&cosh_dir),
        home.join(".copilot-shell/cosh/hook-feedback")
    );
}

#[test]
fn parse_language_setting_accepts_canonical_values_and_aliases() {
    assert_eq!(
        parse_language_setting("auto").map(LanguageSetting::as_config_value),
        Some("auto")
    );
    assert_eq!(
        parse_language_setting("en_US").map(LanguageSetting::as_config_value),
        Some("en-US")
    );
    assert_eq!(
        parse_language_setting("zh-Hans").map(LanguageSetting::as_config_value),
        Some("zh-CN")
    );
    assert_eq!(parse_language_setting("fr-FR"), None);
}

#[test]
fn language_setting_from_config_content_tracks_explicit_auto() {
    let content = r#"
[ui]
language = "auto"
"#;
    assert_eq!(
        language_setting_from_config_content(content).as_deref(),
        Some("auto")
    );
}

#[test]
fn language_setting_from_config_content_reads_simple_value() {
    let content = "ui.language = en-US\n";
    assert_eq!(
        language_setting_from_config_content(content).as_deref(),
        Some("en-US")
    );
}

#[test]
fn language_setting_from_config_content_ignores_invalid_language() {
    let content = r#"
[ui]
language = "fr-FR"
"#;
    assert_eq!(language_setting_from_config_content(content), None);
}

#[test]
fn parse_simple_key_value() {
    let content = r#"
shell.default = "zsh"
shell.integration = enhanced
shell.analysis_mode = conservative
shell.approval_mode = recommend
shell.adapter_default = "qwen"
ui.language = zh-CN
"#;
    let mut cfg = CoshConfig::default();
    parse_simple_config(content, &mut cfg);
    assert_eq!(cfg.shell_default, "zsh");
    assert_eq!(cfg.shell_integration, "enhanced");
    assert_eq!(cfg.analysis_mode, "conservative");
    assert_eq!(cfg.approval_mode, CoshApprovalMode::Recommend);
    assert_eq!(cfg.adapter_default, "qwen");
    assert_eq!(cfg.language, "zh-CN");

    for value in ["balanced", "strict", "suggest"] {
        let mut simple = CoshConfig::default();
        parse_simple_config(&format!("shell.approval_mode = {value}\n"), &mut simple);
        assert_eq!(simple.approval_mode, CoshApprovalMode::Recommend);

        let mut toml = CoshConfig::default();
        parse_toml_config(
            &format!("[shell]\napproval_mode = \"{value}\"\n"),
            &mut toml,
        );
        assert_eq!(toml.approval_mode, CoshApprovalMode::Recommend);
    }

    let mut config = CoshConfig {
        approval_mode: CoshApprovalMode::Trust,
        ..Default::default()
    };
    parse_simple_config("shell.approval_mode = invalid\n", &mut config);
    assert_eq!(config.approval_mode, CoshApprovalMode::Recommend);
}

#[test]
fn parse_ignores_comments_and_blank_lines() {
    let content = "# this is a comment\n\nshell.default = fish\n";
    let mut cfg = CoshConfig::default();
    parse_simple_config(content, &mut cfg);
    assert_eq!(cfg.shell_default, "fish");
    assert_eq!(cfg.analysis_mode, "smart");
}

#[test]
fn parse_boolean_fields() {
    let content = "ui.startup_banner = false\nui.startup_hooks = true\nui.debug = on\n";
    let mut cfg = CoshConfig::default();
    parse_simple_config(content, &mut cfg);
    assert!(!cfg.startup_banner);
    assert!(cfg.startup_hooks);
    assert!(cfg.debug);
}

// #2161: both config forms must reach `input_wait_timeout_secs`; invalid
// values are treat-as-unset (built-in default), never 0/disabled.
#[test]
fn parse_input_wait_timeout_simple_key() {
    let mut cfg = CoshConfig::default();
    parse_simple_config("shell.input_wait_timeout_secs = 15\n", &mut cfg);
    assert_eq!(cfg.input_wait_timeout_secs, 15);

    let mut cfg = CoshConfig::default();
    parse_simple_config("shell.input_wait_timeout_secs = 0\n", &mut cfg);
    assert_eq!(cfg.input_wait_timeout_secs, 0);

    let mut cfg = CoshConfig::default();
    parse_simple_config("shell.input_wait_timeout_secs = nonsense\n", &mut cfg);
    assert_eq!(cfg.input_wait_timeout_secs, 120);
}

#[test]
fn parse_input_wait_timeout_shell_table() {
    let mut cfg = CoshConfig::default();
    parse_toml_config("[shell]\ninput_wait_timeout_secs = 15\n", &mut cfg);
    assert_eq!(cfg.input_wait_timeout_secs, 15);

    let mut cfg = CoshConfig::default();
    parse_toml_config("[shell]\ninput_wait_timeout_secs = 0\n", &mut cfg);
    assert_eq!(cfg.input_wait_timeout_secs, 0);

    // Negative and non-integer values are treat-as-unset.
    let mut cfg = CoshConfig::default();
    parse_toml_config("[shell]\ninput_wait_timeout_secs = -5\n", &mut cfg);
    assert_eq!(cfg.input_wait_timeout_secs, 120);

    let mut cfg = CoshConfig::default();
    parse_toml_config("[shell]\ninput_wait_timeout_secs = \"soon\"\n", &mut cfg);
    assert_eq!(cfg.input_wait_timeout_secs, 120);
}

#[test]
fn parse_toml_ui_language_and_booleans() {
    let content = r#"
[ui]
language = "en-US"
startup_banner = false
startup_hooks = true
debug = true
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);
    assert_eq!(cfg.language, "en-US");
    assert!(!cfg.startup_banner);
    assert!(cfg.startup_hooks);
    assert!(cfg.debug);
}

#[test]
fn parse_toml_adapter_default() {
    let content = r#"
[shell]
adapter_default = "qwen"
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);
    assert_eq!(cfg.adapter_default, "qwen");
}

#[test]
fn parse_simple_health_flags() {
    let content = r#"
health.enabled = false
health.role = mysql-primary
health.memory_sensitive = true
health.verbose = on
"#;
    let mut cfg = CoshConfig::default();
    parse_simple_config(content, &mut cfg);

    assert!(!cfg.health.enabled);
    assert_eq!(cfg.health.role.as_deref(), Some("mysql-primary"));
    assert!(cfg.health.memory_sensitive);
    assert!(cfg.health.verbose);
    assert_eq!(cfg.health.critical_mounts, vec!["/"]);
    assert!(cfg.health.services.is_empty());
}

#[test]
fn parse_toml_health_config() {
    let content = r#"
[health]
enabled = true
role = "mysql-primary"
memory_sensitive = true
critical_mounts = ["/", "/data"]
verbose = false

[[health.services]]
name = "mysql"
expected = "active"

[[health.services]]
name = "backup-worker"
expected = "inactive"
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);

    assert!(cfg.health.enabled);
    assert_eq!(cfg.health.role.as_deref(), Some("mysql-primary"));
    assert!(cfg.health.memory_sensitive);
    assert_eq!(cfg.health.critical_mounts, vec!["/", "/data"]);
    assert!(!cfg.health.verbose);
    assert_eq!(cfg.health.services.len(), 2);
    assert_eq!(cfg.health.services[0].name, "mysql");
    assert_eq!(
        cfg.health.services[0].expected,
        HealthServiceExpectedState::Active
    );
    assert_eq!(cfg.health.services[1].name, "backup-worker");
    assert_eq!(
        cfg.health.services[1].expected,
        HealthServiceExpectedState::Inactive
    );
}

#[test]
fn parse_toml_health_services_ignore_invalid_entries() {
    let content = r#"
[health]
critical_mounts = []

[[health.services]]
name = ""
expected = "active"

[[health.services]]
name = "redis"
expected = "unexpected"
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);

    assert_eq!(cfg.health.critical_mounts, vec!["/"]);
    assert_eq!(cfg.health.services.len(), 1);
    assert_eq!(cfg.health.services[0].name, "redis");
    assert_eq!(
        cfg.health.services[0].expected,
        HealthServiceExpectedState::Active
    );
}

#[test]
fn write_language_config_creates_minimal_toml_and_reload_reads_it() {
    let path = temp_config_path("create-language");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());

    write_language_config_to_path(&path, "zh-CN").expect("write language");

    let content = std::fs::read_to_string(&path).expect("read config");
    assert!(content.contains("[ui]"), "{content}");
    assert!(content.contains("language = \"zh-CN\""), "{content}");
    let mut cfg = CoshConfig::default();
    load_config_file_into(&path, &mut cfg);
    assert_eq!(cfg.language, "zh-CN");

    let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
}

#[test]
fn write_language_config_updates_toml_and_preserves_unrelated_values() {
    let path = temp_config_path("update-language");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create config dir");
    std::fs::write(
        &path,
        r#"
[shell]
adapter_default = "qwen"

[ui]
startup_banner = false
language = "auto"
"#,
    )
    .expect("write existing config");

    write_language_config_to_path(&path, "en").expect("update language");

    let content = std::fs::read_to_string(&path).expect("read config");
    assert!(content.contains("[shell]"), "{content}");
    assert!(content.contains("adapter_default = \"qwen\""), "{content}");
    assert!(content.contains("startup_banner = false"), "{content}");
    assert!(content.contains("language = \"en-US\""), "{content}");
    let mut cfg = CoshConfig::default();
    load_config_file_into(&path, &mut cfg);
    assert_eq!(cfg.language, "en-US");

    let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
}

#[test]
fn write_language_config_persists_auto_literal() {
    let path = temp_config_path("auto-language");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());

    write_language_config_to_path(&path, "auto").expect("write auto language");

    let content = std::fs::read_to_string(&path).expect("read config");
    assert!(content.contains("language = \"auto\""), "{content}");
    let mut cfg = CoshConfig::default();
    load_config_file_into(&path, &mut cfg);
    assert_eq!(cfg.language, "auto");

    let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
}

#[test]
fn write_language_config_refuses_invalid_or_simple_config() {
    let path = temp_config_path("invalid-language");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create config dir");
    std::fs::write(&path, "ui.language = zh-CN\n").expect("write simple config");

    let err = write_language_config_to_path(&path, "zh-CN").expect_err("reject invalid TOML");
    assert!(err.contains("edit ui.language manually"), "{err}");
    let content = std::fs::read_to_string(&path).expect("read config");
    assert_eq!(content, "ui.language = zh-CN\n");
    let err = write_language_config_to_path(&path, "fr-FR").expect_err("reject language");
    assert!(err.contains("supported: auto, en-US, zh-CN"), "{err}");

    let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
}

#[test]
fn parse_unknown_keys_ignored() {
    let content = "unknown.key = value\nshell.default = dash\n";
    let mut cfg = CoshConfig::default();
    parse_simple_config(content, &mut cfg);
    assert_eq!(cfg.shell_default, "dash");
}

#[test]
fn parse_trusted_commands_accumulates() {
    let content = r#"
shell.trusted_command = "npm test"
shell.trusted_command = "make"
shell.trusted_command = "cargo build"
"#;
    let mut cfg = CoshConfig::default();
    parse_simple_config(content, &mut cfg);
    assert_eq!(cfg.trusted_commands.len(), 3);
    assert_eq!(cfg.trusted_commands[0], "npm test");
    assert_eq!(cfg.trusted_commands[1], "make");
    assert_eq!(cfg.trusted_commands[2], "cargo build");
}

#[test]
fn parse_trusted_command_ignores_empty() {
    let content = "shell.trusted_command = \"\"\nshell.trusted_command = \"git status\"\n";
    let mut cfg = CoshConfig::default();
    parse_simple_config(content, &mut cfg);
    assert_eq!(cfg.trusted_commands.len(), 1);
    assert_eq!(cfg.trusted_commands[0], "git status");
}

#[test]
fn parse_trusted_project_roots_accumulates() {
    let content = r#"
shell.trusted_project_root = "/work/app"
shell.trusted_project_root = "/work/lib"
shell.trusted_project_root = ""
"#;
    let mut cfg = CoshConfig::default();
    parse_simple_config(content, &mut cfg);
    assert_eq!(cfg.trusted_project_roots.len(), 2);
    assert_eq!(cfg.trusted_project_roots[0], PathBuf::from("/work/app"));
    assert_eq!(cfg.trusted_project_roots[1], PathBuf::from("/work/lib"));
}

#[test]
fn parse_toml_trusted_project_roots() {
    let content = r#"
[shell]
trusted_project_roots = ["/work/app", "/work/lib"]
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);
    assert_eq!(cfg.trusted_project_roots.len(), 2);
    assert_eq!(cfg.trusted_project_roots[0], PathBuf::from("/work/app"));
    assert_eq!(cfg.trusted_project_roots[1], PathBuf::from("/work/lib"));
    assert!(cfg.readonly.errors.is_empty(), "{:?}", cfg.readonly.errors);
}

#[test]
fn project_trust_store_adds_dedupes_and_removes_canonical_roots() {
    let root = std::env::temp_dir().join("cosh-shell-trust-store-project");
    let nested = root.join("nested");
    let other = std::env::temp_dir().join("cosh-shell-trust-store-other");
    let store =
        std::env::temp_dir().join(format!("cosh-shell-trust-store-{}.txt", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&other);
    let _ = std::fs::remove_file(&store);
    std::fs::create_dir_all(&nested).expect("create nested");
    std::fs::create_dir_all(&other).expect("create other");

    add_trusted_project_root_to_store_path(&store, &root).expect("trust root");
    add_trusted_project_root_to_store_path(&store, &nested.join("..")).expect("dedupe root");
    add_trusted_project_root_to_store_path(&store, &other).expect("trust other");

    let roots = read_trusted_project_roots_from_store_path(&store).expect("read store");
    assert_eq!(roots.len(), 2, "{roots:?}");
    assert!(roots.contains(&root.canonicalize().expect("canonical root")));
    assert!(roots.contains(&other.canonicalize().expect("canonical other")));

    remove_trusted_project_root_from_store_path(&store, &nested.join("..")).expect("untrust root");
    let roots = read_trusted_project_roots_from_store_path(&store).expect("read store");
    assert_eq!(roots, vec![other.canonicalize().expect("canonical other")]);

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&other);
    let _ = std::fs::remove_file(&store);

    assert_corrupted_project_trust_store_is_reported();
    #[cfg(unix)]
    assert_trust_store_atomic_write_preserves_permissions();
}

#[test]
fn load_project_trust_store_extends_config_roots() {
    let store = std::env::temp_dir().join(format!(
        "cosh-shell-trust-store-load-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store);
    std::fs::write(&store, "# comment\n/work/app\n\n/work/lib\n").expect("write store");

    let mut cfg = CoshConfig::default();
    load_project_trust_store(&mut cfg, &store).expect("load store");

    assert_eq!(cfg.trusted_project_roots.len(), 2);
    assert_eq!(cfg.trusted_project_roots[0], PathBuf::from("/work/app"));
    assert_eq!(cfg.trusted_project_roots[1], PathBuf::from("/work/lib"));

    let _ = std::fs::remove_file(&store);
}

#[test]
fn clear_project_trust_store_path_keeps_empty_store_file() {
    let store = std::env::temp_dir().join(format!(
        "cosh-shell-trust-store-clear-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store);
    std::fs::write(&store, "/work/app\n/work/lib\n").expect("write store");

    write_trusted_project_roots_to_store_path(&store, &[]).expect("clear store");

    let roots = read_trusted_project_roots_from_store_path(&store).expect("read store");
    assert!(roots.is_empty(), "{roots:?}");
    let content = std::fs::read_to_string(&store).expect("read store");
    assert!(content.contains("cosh-shell trusted project hook roots"));

    let _ = std::fs::remove_file(&store);
}

fn assert_corrupted_project_trust_store_is_reported() {
    let store = std::env::temp_dir().join(format!(
        "cosh-shell-trust-store-corrupt-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store);
    std::fs::write(&store, b"/work/app\n\xff\n").expect("write corrupt store");

    let error = read_trusted_project_roots_from_store_path(&store)
        .expect_err("invalid UTF-8 must not become an empty trust set");
    assert!(error.contains("not valid UTF-8"), "{error}");
    let mut cfg = CoshConfig::default();
    assert!(load_project_trust_store(&mut cfg, &store).is_err());
    assert!(cfg.trusted_project_roots.is_empty());

    let _ = std::fs::remove_file(&store);
}

#[cfg(unix)]
fn assert_trust_store_atomic_write_preserves_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-trust-store-atomic-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let store = dir.join("trusted-project-hooks");
    std::fs::write(&store, "/work/old\n").expect("seed store");
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o640))
        .expect("set store mode");

    write_trusted_project_roots_to_store_path(&store, &[PathBuf::from("/work/new")])
        .expect("atomic update");

    let mode = std::fs::metadata(&store)
        .expect("stat store")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o640);
    assert_eq!(
        read_trusted_project_roots_from_store_path(&store).expect("read updated store"),
        vec![PathBuf::from("/work/new")]
    );
    assert!(!std::fs::read_dir(&dir)
        .expect("list store directory")
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().contains(".tmp-")));

    let link = dir.join("trusted-project-hooks-link");
    symlink(&store, &link).expect("create symlink");
    let error = write_trusted_project_roots_to_store_path(&link, &[])
        .expect_err("symlink target must not be replaced");
    assert!(error.contains("symbolic link"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hook_feedback_store_overwrites_key_and_ignores_invalid_entries() {
    let store = std::env::temp_dir().join(format!(
        "cosh-shell-hook-feedback-store-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store);
    std::fs::write(
        &store,
        "# comment\nbad-line\nloud\tmemory:bad\nnoisy\tmemory:old\n",
    )
    .expect("write malformed store");

    let mut noisy = HookFeedbackPreference::minimal("memory:pressure:free", "noisy");
    noisy.topic = "memory".to_string();
    noisy.entity_key = "system-memory".to_string();
    noisy.severity = "critical".to_string();
    noisy.command_intent = "free".to_string();
    noisy.recorded_at_ms = 100;
    noisy.window_ms = 600000;
    write_hook_feedback_to_store_path(&store, noisy).expect("write noisy feedback");
    let mut useful = HookFeedbackPreference::minimal("memory:pressure:free", "useful");
    useful.topic = "memory".to_string();
    useful.entity_key = "system-memory".to_string();
    useful.severity = "critical".to_string();
    useful.command_intent = "free".to_string();
    useful.recorded_at_ms = 200;
    useful.window_ms = 600000;
    write_hook_feedback_to_store_path(&store, useful).expect("overwrite feedback");

    let entries = read_hook_feedback_from_store_path(&store);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0].suppression_key, "memory:old");
    assert_eq!(entries[0].label, "noisy");
    assert_eq!(entries[1].suppression_key, "memory:pressure:free");
    assert_eq!(entries[1].label, "useful");
    assert_eq!(entries[1].topic, "memory");
    assert_eq!(entries[1].entity_key, "system-memory");
    assert_eq!(entries[1].severity, "critical");
    assert_eq!(entries[1].command_intent, "free");
    assert_eq!(entries[1].recorded_at_ms, 200);
    assert_eq!(entries[1].window_ms, 600000);
    let content = std::fs::read_to_string(&store).expect("read feedback store");
    assert!(
        content.contains("format: label<TAB>suppression_key<TAB>key=value"),
        "{content}"
    );
    assert!(content.contains("topic=memory"), "{content}");
    assert!(content.contains("entity=system-memory"), "{content}");
    assert!(content.contains("severity=critical"), "{content}");
    assert!(content.contains("intent=free"), "{content}");
    assert!(content.contains("recorded_at_ms=200"), "{content}");
    assert!(content.contains("window_ms=600000"), "{content}");
    assert!(!content.contains("bad-line"), "{content}");
    assert!(!content.contains("loud\tmemory:bad"), "{content}");

    let _ = std::fs::remove_file(&store);
}

#[test]
fn clear_hook_feedback_store_path_keeps_empty_store_file() {
    let store = std::env::temp_dir().join(format!(
        "cosh-shell-hook-feedback-clear-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store);
    std::fs::write(&store, "noisy\tmemory:pressure:free\n").expect("write store");

    write_hook_feedback_entries_to_store_path(&store, &[]).expect("clear feedback store");

    let entries = read_hook_feedback_from_store_path(&store);
    assert!(entries.is_empty(), "{entries:?}");
    let content = std::fs::read_to_string(&store).expect("read store");
    assert!(content.contains("cosh-shell hook feedback"));

    let _ = std::fs::remove_file(&store);
}

#[test]
fn parse_toml_readonly_dsl_adds_generic_override_and_disabled_rules() {
    let content = r#"
[shell]
readonly_disabled = ["git branch", "docker inspect"]

[shell.readonly.mytool]
type = "generic"
short_flags = "v"
long_flags = ["--verbose"]
value_flags = [["-n", 10], { flag = "--count", max = 10 }]
deny_flags = ["--write"]
path_mode = "required"
bare_number_max = 5
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);

    assert_eq!(cfg.readonly.disabled.len(), 2);
    assert_eq!(cfg.readonly.disabled[0].command, "git");
    assert_eq!(
        cfg.readonly.disabled[0].subcommand.as_deref(),
        Some("branch")
    );
    assert_eq!(cfg.readonly.overrides.len(), 1);
    assert_eq!(cfg.readonly.overrides[0].command, "mytool");
    assert!(cfg.readonly.errors.is_empty(), "{:?}", cfg.readonly.errors);
}

#[test]
fn parse_toml_readonly_dsl_adds_subcommand_override() {
    let content = r#"
[shell.readonly.safegit]
type = "subcommand"
deny_args = ["-c"]

[shell.readonly.safegit.subcommands.status]
type = "generic"
long_flags = ["--short"]
path_mode = "none"
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);

    assert_eq!(cfg.readonly.overrides.len(), 1);
    assert!(cfg.readonly.errors.is_empty(), "{:?}", cfg.readonly.errors);
}

#[test]
fn parse_toml_readonly_dsl_records_invalid_rules_fail_closed() {
    let content = r#"
[shell.readonly.bad]
type = "generic"
path_mode = "somewhere"
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);

    assert!(cfg.readonly.overrides.is_empty());
    assert_eq!(cfg.readonly.errors.len(), 1);
}

#[test]
fn parse_toml_readonly_dsl_records_parse_error_fail_closed() {
    let content = r#"
[shell]
readonly_disabled = [

[shell.readonly.bad]
type = "bare"
"#;
    let mut cfg = CoshConfig::default();
    parse_toml_config(content, &mut cfg);

    assert!(cfg.readonly.disabled.is_empty());
    assert!(cfg.readonly.overrides.is_empty());
    assert_eq!(cfg.readonly.errors.len(), 1);
}
