use super::*;

#[test]
fn raw_cli_cosh_core_bash_ordinary_commands_passthrough_without_agent() {
    let home = temp_shell_home("cosh-core-bash-passthrough");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_and_env(
        "cosh-core",
        &["--shell", "bash"],
        "printf 'bash-pwd:%s\\n' \"$PWD\"\necho cosh-pass-bash\nexit\n",
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", "/tmp/cosh-core-should-not-start"),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("bash-pwd:"), "{output}");
    assert!(output.contains("cosh-pass-bash"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert_no_approval_request_card(&output);
    assert!(!output.contains("failed to run cosh-core"), "{output}");
}

#[test]
fn raw_cli_cosh_core_zsh_ordinary_commands_passthrough_without_agent() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_shell_home("cosh-core-zsh-passthrough");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_and_env(
        "cosh-core",
        &["--shell", "zsh"],
        "print -r -- zsh-pwd:$PWD\necho cosh-pass-zsh\nexit\n",
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", "/tmp/cosh-core-should-not-start"),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("zsh-pwd:"), "{output}");
    assert!(output.contains("cosh-pass-zsh"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert_no_approval_request_card(&output);
    assert!(!output.contains("failed to run cosh-core"), "{output}");
}
