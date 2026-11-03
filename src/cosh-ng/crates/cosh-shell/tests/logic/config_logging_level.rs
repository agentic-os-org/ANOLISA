//! Exercises the production `load_config` path for the shared
//! `[logging].level` key through the public crate API. The coverage lives
//! in the logic integration layer on purpose: the config module is
//! compiled into both the lib and bin targets, so unit tests there would
//! grow the lib/bin overlap audited by scripts/check-test-inventory.sh.

use std::env;
use std::fs;

/// Single test in this file so the process-local HOME swap below cannot
/// race with sibling tests.
#[test]
fn load_config_resolves_log_level_keys() {
    let home = env::temp_dir().join(format!("cosh-config-logging-level-{}", std::process::id()));
    let config_dir = home.join(".copilot-shell");
    let config_path = config_dir.join("config.toml");
    fs::create_dir_all(&config_dir).expect("create config dir");

    let saved_home = env::var("HOME").ok();
    let saved_cosh_log = env::var("COSH_LOG").ok();
    env::set_var("HOME", &home);
    // COSH_LOG is the highest-priority override in load_config; drop it so
    // the config file under test decides the outcome.
    env::remove_var("COSH_LOG");

    let cases: &[(&str, &str, &str)] = &[
        // The shared key wins over the legacy fallback in both table and
        // dotted forms, regardless of which key appears first in the file.
        (
            "[logging]\nlevel = \"warn\"\n\n[ui]\nlog_level = \"debug\"\n",
            "warn",
            "toml form, shared key first",
        ),
        (
            "[ui]\nlog_level = \"debug\"\n\n[logging]\nlevel = \"warn\"\n",
            "warn",
            "toml form, legacy key first",
        ),
        (
            "logging.level = \"warn\"\nui.log_level = \"debug\"\n",
            "warn",
            "dotted form, shared key first",
        ),
        (
            "ui.log_level = \"debug\"\nlogging.level = \"warn\"\n",
            "warn",
            "dotted form, legacy key first",
        ),
        // The shared key alone must apply; cosh-core reads the same key.
        (
            "[logging]\nlevel = \"trace\"\n",
            "trace",
            "toml form, shared key only",
        ),
        // The legacy key remains a fallback for existing user configs.
        (
            "[ui]\nlog_level = \"warn\"\n",
            "warn",
            "toml form, legacy key only",
        ),
        (
            "ui.log_level = \"warn\"\n",
            "warn",
            "dotted form, legacy key only",
        ),
    ];

    for (content, expected, label) in cases {
        fs::write(&config_path, content).expect("write config");
        let config = cosh_shell::load_config();
        assert_eq!(config.log_level, *expected, "case: {label}");
    }

    match saved_home {
        Some(value) => env::set_var("HOME", value),
        None => env::remove_var("HOME"),
    }
    if let Some(value) = saved_cosh_log {
        env::set_var("COSH_LOG", value);
    }
    let _ = fs::remove_dir_all(&home);
}
