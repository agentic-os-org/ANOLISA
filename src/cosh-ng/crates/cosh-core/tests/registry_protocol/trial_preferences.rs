use super::*;
use std::fs;
use std::os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt, PermissionsExt};

fn preferences_path(home: &Path) -> std::path::PathBuf {
    home.join(".copilot-shell/cosh/preferences.toml")
}

fn private_directory(path: &Path) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .unwrap();
}

fn write_preferences(home: &Path, content: &str) {
    let path = preferences_path(home);
    private_directory(path.parent().unwrap());
    fs::write(&path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn state(home: &Path) -> Value {
    run_registry_request_with_context("auth", "state", Value::Null, home, None)
}

fn set(home: &Path, auto_use: Value) -> Value {
    run_registry_request_with_context(
        "auth",
        "set_trial_preference",
        serde_json::json!({ "auto_use": auto_use }),
        home,
        None,
    )
}

#[test]
fn missing_preferences_default_to_auto_use_without_creating_file() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        state(home.path())["data"]["trial_preferences"],
        serde_json::json!({"auto_use": true})
    );
    assert!(!preferences_path(home.path()).exists());
}

#[test]
fn writable_preferences_fail_closed_and_are_not_overwritten() {
    for mode in [0o620, 0o602, 0o666] {
        let home = tempfile::tempdir().unwrap();
        let document = "version = 1\n[ecs_trial]\nauto_use = true\n";
        write_preferences(home.path(), document);
        let path = preferences_path(home.path());
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let response = state(home.path());
        assert_eq!(
            response["data"]["trial_preferences"],
            serde_json::json!({
                "auto_use":false,"error_code":"trial_preferences_unsafe_path"
            }),
            "{mode:o}: {response}"
        );
        let response = set(home.path(), false.into());
        assert_eq!(response["success"], false, "{mode:o}: {response}");
        assert_eq!(
            response["data"]["error_code"],
            "trial_preferences_unsafe_path"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), document);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
}

#[test]
fn saves_false_then_true_across_registry_processes() {
    let home = tempfile::tempdir().unwrap();
    for value in [false, true] {
        let response = set(home.path(), value.into());
        assert_eq!(response["success"], true, "{response}");
        assert_eq!(response["data"]["trial_preferences"]["auto_use"], value);
        assert_eq!(
            state(home.path())["data"]["trial_preferences"]["auto_use"],
            value
        );
    }
    let path = preferences_path(home.path());
    let document: toml::Value = fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(document["version"].as_integer(), Some(1));
    assert_eq!(document["ecs_trial"]["auto_use"].as_bool(), Some(true));
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for path in [
        home.path().join(".copilot-shell"),
        home.path().join(".copilot-shell/cosh"),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn save_preserves_unknown_fields_and_missing_auto_use_defaults_true() {
    let home = tempfile::tempdir().unwrap();
    write_preferences(
        home.path(),
        "version = 1\nfuture = 'keep'\n[ecs_trial]\nother = 42\n[unrelated]\nvalue = true\n",
    );
    assert_eq!(
        state(home.path())["data"]["trial_preferences"]["auto_use"],
        true
    );
    assert_eq!(set(home.path(), false.into())["success"], true);
    let document: toml::Value = fs::read_to_string(preferences_path(home.path()))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(document["future"].as_str(), Some("keep"));
    assert_eq!(document["ecs_trial"]["other"].as_integer(), Some(42));
    assert_eq!(document["unrelated"]["value"].as_bool(), Some(true));
    assert_eq!(document["ecs_trial"]["auto_use"].as_bool(), Some(false));
}

#[test]
fn invalid_preferences_fail_closed_and_are_not_overwritten() {
    for (document, code) in [
        ("secret-marker = [", "trial_preferences_invalid"),
        (
            "version = 2\n[ecs_trial]\nauto_use = true\n",
            "trial_preferences_version",
        ),
        (
            "version = 1\n[ecs_trial]\nauto_use = 'true'\n",
            "trial_preferences_invalid",
        ),
        (
            "version = 1\necs_trial = false\n",
            "trial_preferences_invalid",
        ),
        (
            "[ecs_trial]\nauto_use = true\n",
            "trial_preferences_version",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        write_preferences(home.path(), document);
        let response = state(home.path());
        assert_eq!(
            response["data"]["trial_preferences"],
            serde_json::json!({"auto_use": false, "error_code": code})
        );
        let response = set(home.path(), false.into());
        assert_eq!(response["success"], false, "{response}");
        assert_eq!(response["data"]["error_code"], code);
        assert!(!response.to_string().contains("secret-marker"));
        assert_eq!(
            fs::read_to_string(preferences_path(home.path())).unwrap(),
            document
        );
    }
}

#[test]
fn non_boolean_set_is_rejected_without_creating_preferences() {
    let home = tempfile::tempdir().unwrap();
    for invalid in [Value::Null, "false".into(), 0.into()] {
        let response = set(home.path(), invalid);
        assert_eq!(response["success"], false);
        assert_eq!(
            response["data"]["error_code"],
            "trial_preferences_invalid_argument"
        );
    }
    assert!(!preferences_path(home.path()).exists());
}

#[test]
fn preferences_and_lock_symlinks_fail_closed() {
    for filename in ["preferences.toml", "preferences.lock"] {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".copilot-shell/cosh");
        private_directory(&directory);
        let target = home.path().join("outside");
        fs::write(&target, "version = 1\n[ecs_trial]\nauto_use = true\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, directory.join(filename)).unwrap();
        assert_eq!(
            state(home.path())["data"]["trial_preferences"]["auto_use"],
            false
        );
        assert_eq!(set(home.path(), false.into())["success"], false);
        assert!(fs::read_to_string(target)
            .unwrap()
            .contains("auto_use = true"));
    }
}

#[test]
fn preference_directory_symlink_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    private_directory(&home.path().join(".copilot-shell"));
    symlink(target.path(), home.path().join(".copilot-shell/cosh")).unwrap();
    assert_eq!(
        state(home.path())["data"]["trial_preferences"]["auto_use"],
        false
    );
    assert_eq!(set(home.path(), false.into())["success"], false);
    assert!(!target.path().join("preferences.toml").exists());
}

#[test]
fn non_regular_preference_and_lock_files_are_rejected() {
    for filename in ["preferences.toml", "preferences.lock"] {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".copilot-shell/cosh").join(filename);
        private_directory(&path);
        assert_eq!(
            state(home.path())["data"]["trial_preferences"]["auto_use"],
            false
        );
        assert_eq!(set(home.path(), false.into())["success"], false);
    }
}

#[test]
fn unwritable_preference_directory_returns_real_failure() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".copilot-shell/cosh");
    private_directory(&directory);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    let response = set(home.path(), false.into());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(response["success"], false, "{response}");
    assert_eq!(response["data"]["error_code"], "trial_preferences_io");
    assert!(!preferences_path(home.path()).exists());
}

#[test]
fn held_lock_has_bounded_wait_and_does_not_overwrite_preferences() {
    let home = tempfile::tempdir().unwrap();
    write_preferences(home.path(), "version = 1\n[ecs_trial]\nauto_use = true\n");
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(home.path().join(".copilot-shell/cosh/preferences.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let started = Instant::now();
    let response = set(home.path(), false.into());
    assert_eq!(response["success"], false);
    assert_eq!(response["data"]["error_code"], "trial_preferences_busy");
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert!(started.elapsed() < Duration::from_secs(6));
    assert!(fs::read_to_string(preferences_path(home.path()))
        .unwrap()
        .contains("auto_use = true"));
}

#[test]
fn concurrent_saves_preserve_unrelated_fields_and_valid_toml() {
    let home = tempfile::tempdir().unwrap();
    write_preferences(home.path(), "version = 1\nfuture = 'keep'\n");
    std::thread::scope(|scope| {
        for index in 0..6 {
            let home = home.path();
            scope.spawn(move || {
                let response = set(home, (index % 2 == 0).into());
                assert_eq!(response["success"], true, "{response}");
            });
        }
    });
    let document: toml::Value = fs::read_to_string(preferences_path(home.path()))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(document["future"].as_str(), Some("keep"));
    assert!(document["ecs_trial"]["auto_use"].is_bool());
}

#[test]
fn missing_home_fails_closed_without_cwd_fallback() {
    let cwd = tempfile::tempdir().unwrap();
    for action in ["state", "set_trial_preference"] {
        let mut command = Command::new(binary_path());
        command
            .arg("--registry")
            .current_dir(cwd.path())
            .env_remove("HOME")
            .env_remove("COSH_HOME")
            .env_remove("COSH_STATES_DIR")
            .env_remove("XDG_DATA_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let response = registry_command_response(
            command,
            serde_json::json!({"type":"registry_request", "request_id":"no-home", "domain":"auth", "action":action, "params":{"auto_use":false}}),
        );
        if action == "state" {
            assert_eq!(
                response["data"]["trial_preferences"],
                serde_json::json!({"auto_use":false, "error_code":"trial_preferences_home_unavailable"})
            );
        } else {
            assert_eq!(response["success"], false);
            assert_eq!(
                response["data"]["error_code"],
                "trial_preferences_home_unavailable"
            );
        }
    }
    assert!(!cwd
        .path()
        .join(".copilot-shell/cosh/preferences.toml")
        .exists());
}

#[test]
fn fifo_and_hardlinked_files_are_rejected_without_blocking() {
    for filename in ["preferences.toml", "preferences.lock"] {
        for fifo in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let directory = home.path().join(".copilot-shell/cosh");
            private_directory(&directory);
            let path = directory.join(filename);
            if fifo {
                nix::unistd::mkfifo(
                    &path,
                    nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
                )
                .unwrap();
            } else {
                let target = home.path().join("outside");
                fs::write(&target, "version = 1\n[ecs_trial]\nauto_use = true\n").unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                fs::hard_link(target, &path).unwrap();
            }
            let started = Instant::now();
            assert_eq!(
                state(home.path())["data"]["trial_preferences"]["auto_use"],
                false
            );
            assert_eq!(set(home.path(), false.into())["success"], false);
            assert!(started.elapsed() < Duration::from_secs(5));
        }
    }
}

#[test]
fn save_rejects_size_growth_that_would_make_preferences_unreadable() {
    let home = tempfile::tempdir().unwrap();
    let document = format!("version=1\nfuture='{}'\n", "x".repeat(65_500));
    assert!(document.len() < 65_536);
    write_preferences(home.path(), &document);
    assert_eq!(
        state(home.path())["data"]["trial_preferences"]["auto_use"],
        true
    );
    let response = set(home.path(), false.into());
    assert_eq!(response["success"], false, "{response}");
    assert_eq!(response["data"]["error_code"], "trial_preferences_invalid");
    assert_eq!(
        fs::read_to_string(preferences_path(home.path())).unwrap(),
        document
    );
}

#[test]
fn reading_preferences_from_a_writable_shared_directory_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    write_preferences(home.path(), "version = 1\n[ecs_trial]\nauto_use = true\n");
    let directory = home.path().join(".copilot-shell/cosh");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
    let response = state(home.path());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        response["data"]["trial_preferences"],
        serde_json::json!({"auto_use":false, "error_code":"trial_preferences_unsafe_path"})
    );
}

fn registry_with_umask(home: &Path, mask: &str, action: &str, params: Value) -> Value {
    let sentinel = home.join("telemetry-disabled");
    fs::write(&sentinel, "").unwrap();
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "umask \"$1\"; shift; exec \"$@\"",
        "cold-registry",
        mask,
    ]);
    command.arg(binary_path()).args(["--registry", "--bare"]);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("COSH_TELEMETRY_DISABLED_PATH", sentinel);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    registry_command_response(
        command,
        serde_json::json!({
            "type":"registry_request","request_id":"umask-registry","domain":"auth",
            "action":action,"params":params
        }),
    )
}

#[test]
fn cold_registry_under_permissive_umask_reads_and_saves_preferences() {
    for mask in ["0002", "0000"] {
        let home = tempfile::tempdir().unwrap();
        let initial = registry_with_umask(home.path(), mask, "state", Value::Null);
        assert_eq!(
            initial["data"]["trial_preferences"],
            serde_json::json!({"auto_use":true}),
            "{mask}: {initial}"
        );
        assert!(!preferences_path(home.path()).exists());
        for value in [false, true] {
            let saved = registry_with_umask(
                home.path(),
                mask,
                "set_trial_preference",
                serde_json::json!({"auto_use":value}),
            );
            assert_eq!(saved["success"], true, "{mask}: {saved}");
            let restored = registry_with_umask(home.path(), mask, "state", Value::Null);
            assert_eq!(restored["data"]["trial_preferences"]["auto_use"], value);
        }
        for name in [
            ".copilot-shell",
            ".copilot-shell/logs",
            ".copilot-shell/cosh",
        ] {
            assert_eq!(
                fs::metadata(home.path().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "{mask}: {name}"
            );
        }
        assert_eq!(
            fs::metadata(preferences_path(home.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn cold_registry_under_permissive_umask_rejects_existing_writable_parent() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".copilot-shell");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o775)).unwrap();
    let result = registry_with_umask(home.path(), "0002", "state", Value::Null);
    assert_eq!(
        result["data"]["trial_preferences"]["error_code"],
        "trial_preferences_unsafe_path"
    );
    let result = registry_with_umask(
        home.path(),
        "0002",
        "set_trial_preference",
        serde_json::json!({"auto_use":true}),
    );
    assert_eq!(result["success"], false);
    assert_eq!(
        fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        0o775
    );
    assert!(!preferences_path(home.path()).exists());
}
