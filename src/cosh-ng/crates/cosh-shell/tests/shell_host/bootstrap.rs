use super::{run_scripted_bash, ScriptedInput, ShellHostConfig, ShellIntegration};
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn internal_shell_marker_covers_native_and_enhanced_after_environment_overrides() {
    for integration in [ShellIntegration::Native, ShellIntegration::Enhanced] {
        for native_mode in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let home = directory.path().join("home");
            fs::create_dir(&home).unwrap();
            fs::write(home.join(".bashrc"), "PS1='marker-probe$ '\n").unwrap();
            let captured = directory.path().join("internal-marker");
            let mut config =
                ShellHostConfig::new("internal-marker", directory.path().join("session"))
                    .with_env("HOME", home.display().to_string())
                    .with_env("COSH_INTERNAL_SHELL", "0")
                    .with_env("COSH_TEST_INTERNAL_MARKER", captured.display().to_string());
            config.integration = integration;
            config.native_mode = native_mode;
            config.bash_path = "/bin/bash".into();

            let output = run_scripted_bash(
                &config,
                &[ScriptedInput::Command(
                    "printf '%s' \"${COSH_INTERNAL_SHELL-unset}\" > \"$COSH_TEST_INTERNAL_MARKER\""
                        .into(),
                )],
            )
            .unwrap();

            assert_eq!(output.exit_status, Some(0), "{integration:?}/{native_mode}");
            assert_eq!(
                fs::read(captured).unwrap(),
                b"1",
                "{integration:?}/{native_mode}"
            );
        }
    }
}

#[test]
fn absolute_bash_program_is_used_even_when_path_names_a_different_shell() {
    let directory = tempfile::tempdir().unwrap();
    let selected = directory.path().join("verified-bash");
    let shadow = directory.path().join("bash");
    let captured = directory.path().join("selected");
    for (program, label) in [(&selected, "verified"), (&shadow, "shadow")] {
        fs::write(program, format!("#!/bin/sh\nprintf {label} > \"$COSH_TEST_SELECTED_PROGRAM\"\nexec /bin/bash \"$@\"\n")).unwrap();
        fs::set_permissions(program, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut config = ShellHostConfig::new("verified-login", directory.path().join("session"))
        .with_env("PATH", directory.path().display().to_string())
        .with_env("COSH_TEST_SELECTED_PROGRAM", captured.display().to_string());
    config.integration = ShellIntegration::Native;
    config.native_mode = false;
    config.bash_path = selected.display().to_string();

    let output = run_scripted_bash(&config, &[ScriptedInput::Command(":".into())]).unwrap();

    assert_eq!(output.exit_status, Some(0));
    assert_eq!(fs::read(captured).unwrap(), b"verified");
}
