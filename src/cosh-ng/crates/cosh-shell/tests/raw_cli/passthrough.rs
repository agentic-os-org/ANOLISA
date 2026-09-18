use std::net::{TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;

use super::*;

#[test]
fn raw_cli_double_dash_passthrough_executes_command_directly() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["--", "echo", "ok"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run double dash passthrough");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(stdout.trim(), "ok", "stdout={stdout}\nstderr={stderr}");
    assert!(stderr.is_empty(), "stdout={stdout}\nstderr={stderr}");
}

#[test]
fn raw_cli_double_dash_passthrough_preserves_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["--", "sh", "-c", "exit 43"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run direct command with nonzero exit");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(43),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Agent:"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_double_dash_passthrough_preserves_signal_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");

    for (signal, expected) in [("INT", 130), ("TERM", 143), ("KILL", 137)] {
        let command = format!("kill -{signal} $$");
        let output = raw_cli_command(binary)
            .args(["--", "sh", "-c", command.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run direct command terminated by signal");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(expected),
            "signal={signal}\nstdout={stdout}\nstderr={stderr}"
        );
    }
}

#[test]
fn raw_cli_double_dash_passthrough_preserves_start_failure_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["--", "/definitely/not/a/cosh-shell-command"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run missing direct command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(126),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains("exec /definitely/not/a/cosh-shell-command failed"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_double_dash_passthrough_does_not_capture_child_help_arg() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["--", "printf", "%s\n", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run direct command with child help arg");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(stdout.trim(), "--help", "stdout={stdout}\nstderr={stderr}");
    assert!(
        !stderr.contains("Usage: cosh-shell"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_double_dash_passthrough_requires_command() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg("--")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run missing direct command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains("missing command after --"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_dash_c_passthrough_preserves_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["-c", "exit 42"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run dash-c passthrough with nonzero exit");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Agent:"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_dash_c_passthrough_preserves_signal_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");

    // The passthrough execs the shell in place, so a signal death is
    // observable as a signal death (bash parity), not a 128+n exit code.
    for (signal, expected) in [("INT", 2), ("TERM", 15), ("KILL", 9)] {
        let command = format!("kill -{signal} $$");
        let output = raw_cli_command(binary)
            .args(["-c", command.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run dash-c command terminated by signal");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.signal(),
            Some(expected),
            "signal={signal}\nstdout={stdout}\nstderr={stderr}"
        );
    }
}

#[test]
fn raw_cli_dash_c_passthrough_filters_wrapper_shell_option() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["--shell", "bash", "-c", "echo shell-filter-ok"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run dash-c passthrough with shell option");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("shell-filter-ok"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stderr.contains("invalid option"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stderr.contains("--shell"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_raw_adapter_dash_c_passthrough_executes_without_agent_ui() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["raw", "cosh-core", "-c", "echo raw-adapter-c-ok"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run raw adapter dash-c passthrough");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("raw-adapter-c-ok"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Agent:"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_raw_adapter_dash_c_passthrough_preserves_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["raw", "cosh-core", "-c", "exit 48"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run raw adapter dash-c passthrough with nonzero exit");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(48),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Agent:"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_stdin_passthrough_preserves_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let mut child = raw_cli_command(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stdin passthrough");

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin
            .write_all(b"exit 44\n")
            .expect("write stdin passthrough command");
    }

    let output = child.wait_with_output().expect("wait stdin passthrough");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(44),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Agent:"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_stdin_passthrough_preserves_signal_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");

    // Same exec-in-place parity as the dash-c variant: the shell's signal
    // death reaches the caller as a signal, exactly like invoking bash.
    for (signal, expected) in [("INT", 2), ("TERM", 15), ("KILL", 9)] {
        let mut child = raw_cli_command(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn stdin passthrough");

        {
            let mut stdin = child.stdin.take().expect("child stdin");
            writeln!(stdin, "kill -{signal} $$").expect("write stdin command terminated by signal");
        }

        let output = child.wait_with_output().expect("wait stdin passthrough");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.signal(),
            Some(expected),
            "signal={signal}\nstdout={stdout}\nstderr={stderr}"
        );
    }
}

#[test]
fn raw_cli_login_dash_c_passthrough_executes_without_agent_ui() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["--login", "-c", "echo login-c-ok"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run login dash-c passthrough");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("login-c-ok"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("cosh-osc$"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_login_dash_c_passthrough_preserves_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["--login", "-c", "exit 45"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run login dash-c passthrough with nonzero exit");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(45),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("cosh-osc$"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_login_argv0_dash_c_passthrough_executes_without_agent_ui() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("-cosh-shell")
        .args(["-c", "echo argv0-login-c-ok"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run login argv0 dash-c passthrough");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("argv0-login-c-ok"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("cosh-osc$"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_login_argv0_dash_c_passthrough_preserves_exit_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("-cosh-shell")
        .args(["-c", "exit 46"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run login argv0 dash-c passthrough with nonzero exit");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(46),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("cosh-osc$"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_login_argv0_stdin_passthrough_preserves_exit_status_without_agent_ui() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let mut child = raw_cli_command(binary)
        .arg0("-cosh-shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn login argv0 stdin passthrough");

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin
            .write_all(b"echo argv0-stdin-ok\nexit 47\n")
            .expect("write login argv0 stdin passthrough commands");
    }

    let output = child
        .wait_with_output()
        .expect("wait login argv0 stdin passthrough");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(47),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("argv0-stdin-ok"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("cosh-osc$"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Agent:"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_ai_off_consumes_agent_marker_without_adapter_or_shell_error() {
    let output = run_raw_cli_with_env(
        "fake",
        "?? should not trigger\necho after-ai-off\nexit\n",
        &[("COSH_SHELL_AI", "off"), ("COSH_SHELL_ISOLATED", "1")],
    );

    assert!(output.contains("after-ai-off"), "{output}");
    assert!(!output.contains("Agent:"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("command not found: ??"), "{output}");
    assert!(!output.contains("bash: ??"), "{output}");
}

#[test]
fn raw_cli_cosh_entry_combined_login_flags_reach_bash_with_arg0() {
    // /usr/bin/cosh contract: `-lc` is handed to bash verbatim and `$0`
    // reflects the invocation name, not a hardcoded shell name.
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .args(["-lc", "printf '[%s]' \"$0\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run cosh entry with combined login flags");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("[cosh]"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_cosh_entry_login_argv0_reaches_inner_shell_dollar_zero() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("-cosh")
        .args(["-c", "printf '[%s]' \"$0\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run login cosh entry");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("[-cosh]"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_cosh_entry_invalid_option_is_judged_by_bash() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .arg("--definitely-invalid")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run cosh entry with invalid option");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("Thinking..."),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stderr.contains("usage: cosh-shell"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_cosh_entry_missing_script_file_reports_bash_127() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .arg("/definitely/not/present")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run cosh entry with missing script operand");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(127),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_cosh_entry_isolated_dash_c_ignores_bash_env() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let home = temp_shell_home("isolated-bash-env");
    let bash_env = home.join("bash-env.sh");
    let sourced = home.join("bash-env-sourced");
    fs::write(
        &bash_env,
        format!("printf sourced > '{}'\n", sourced.display()),
    )
    .expect("write BASH_ENV fixture");
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .env("BASH_ENV", &bash_env)
        .args([
            "--isolated",
            "-c",
            "printf '__COMMAND_RAN__ BASH_ENV=<%s>' \"${BASH_ENV-__UNSET__}\"",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run cosh entry with --isolated on the exec path");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("__COMMAND_RAN__"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("BASH_ENV=<__UNSET__>"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(!sourced.exists(), "BASH_ENV was sourced in isolated mode");

    fs::remove_dir_all(home).expect("remove isolated BASH_ENV fixture");
}

#[test]
fn raw_cli_cosh_entry_isolated_posix_bash_ignores_env() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let home = temp_shell_home("isolated-posix-env");
    let env_file = home.join("posix-env.sh");
    let sourced = home.join("posix-env-sourced");
    fs::write(
        &env_file,
        format!("printf sourced > '{}'\n", sourced.display()),
    )
    .expect("write POSIX ENV fixture");

    let run = |isolated: bool| {
        let args = if isolated {
            vec![
                "--isolated",
                "--posix",
                "-i",
                "-c",
                "printf '__ISOLATED_POSIX_RAN__ ENV=<%s>' \"${ENV-__UNSET__}\"",
            ]
        } else {
            vec![
                "--posix",
                "-i",
                "-c",
                "printf '__PLAIN_POSIX_RAN__ ENV=<%s>' \"${ENV-__UNSET__}\"",
            ]
        };
        raw_cli_command(binary)
            .arg0("cosh")
            .env("ENV", &env_file)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run POSIX interactive bash command")
    };

    let plain = run(false);
    let plain_stdout = String::from_utf8_lossy(&plain.stdout);
    let plain_stderr = String::from_utf8_lossy(&plain.stderr);
    assert!(
        plain.status.success(),
        "stdout={plain_stdout}\nstderr={plain_stderr}"
    );
    assert!(
        plain_stdout.contains("__PLAIN_POSIX_RAN__"),
        "stdout={plain_stdout}\nstderr={plain_stderr}"
    );
    assert!(sourced.exists(), "plain POSIX bash did not source ENV");
    fs::remove_file(&sourced).expect("reset POSIX ENV marker");

    let isolated = run(true);
    let isolated_stdout = String::from_utf8_lossy(&isolated.stdout);
    let isolated_stderr = String::from_utf8_lossy(&isolated.stderr);
    assert!(
        isolated.status.success(),
        "stdout={isolated_stdout}\nstderr={isolated_stderr}"
    );
    assert!(
        isolated_stdout.contains("__ISOLATED_POSIX_RAN__ ENV=<__UNSET__>"),
        "stdout={isolated_stdout}\nstderr={isolated_stderr}"
    );
    assert!(!sourced.exists(), "isolated POSIX bash sourced ENV");

    fs::remove_dir_all(home).expect("remove POSIX ENV fixture");
}

#[test]
fn raw_cli_cosh_entry_isolated_zsh_args_fail_closed() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let home = temp_shell_home("isolated-zsh-fail-closed");
    let zshenv_sourced = home.join("zshenv-sourced");
    let env_sourced = home.join("env-sourced");
    let profile_sourced = home.join("profile-sourced");
    let command_ran = home.join("command-ran");
    fs::write(
        home.join(".zshenv"),
        format!("printf sourced > '{}'\n", zshenv_sourced.display()),
    )
    .expect("write Zsh environment fixture");
    let env_file = home.join("sh-env");
    fs::write(
        &env_file,
        format!("printf sourced > '{}'\n", env_sourced.display()),
    )
    .expect("write emulation ENV fixture");
    fs::write(
        home.join(".profile"),
        format!("printf sourced > '{}'\n", profile_sourced.display()),
    )
    .expect("write emulation profile fixture");
    let script = home.join("adversarial.zsh");
    fs::write(
        &script,
        format!("printf '%s' \"$1\" > '{}'\n", command_ran.display()),
    )
    .expect("write Zsh script fixture");

    #[derive(Clone, Copy)]
    enum Startup {
        Zshenv,
        Env,
        Profile,
    }

    let script = script.to_string_lossy().into_owned();
    let command = format!("printf command > '{}'", command_ran.display());
    let cases = vec![
        (
            "plus-bf",
            vec!["+bf".to_string(), script.clone(), "script-arg".to_string()],
            Startup::Zshenv,
            "script-arg",
        ),
        (
            "emulate-zsh",
            vec![
                "--emulate".to_string(),
                "zsh".to_string(),
                "-c".to_string(),
                command.clone(),
            ],
            Startup::Zshenv,
            "command",
        ),
        (
            "emulate-sh-env",
            vec![
                "--emulate".to_string(),
                "sh".to_string(),
                "-i".to_string(),
                "-c".to_string(),
                command.clone(),
            ],
            Startup::Env,
            "command",
        ),
        (
            "emulate-ksh-env",
            vec![
                "--emulate".to_string(),
                "ksh".to_string(),
                "-i".to_string(),
                "-c".to_string(),
                command.clone(),
            ],
            Startup::Env,
            "command",
        ),
        (
            "emulate-sh-profile",
            vec![
                "--emulate".to_string(),
                "sh".to_string(),
                "-l".to_string(),
                "-c".to_string(),
                command.clone(),
            ],
            Startup::Profile,
            "command",
        ),
        (
            "stacked-named",
            vec![
                "-xo".to_string(),
                "RCS".to_string(),
                "-c".to_string(),
                command.clone(),
            ],
            Startup::Zshenv,
            "command",
        ),
        (
            "plus-minus-terminator",
            vec!["+-".to_string(), script.clone(), "script-arg".to_string()],
            Startup::Zshenv,
            "script-arg",
        ),
        (
            "minus-stacked-terminator",
            vec!["-x-".to_string(), script.clone(), "script-arg".to_string()],
            Startup::Zshenv,
            "script-arg",
        ),
        (
            "plus-stacked-terminator",
            vec!["+x-".to_string(), script.clone(), "script-arg".to_string()],
            Startup::Zshenv,
            "script-arg",
        ),
    ];

    for (case, shell_args, startup, expected_command) in cases {
        let clear_markers = || {
            for marker in [
                &zshenv_sourced,
                &env_sourced,
                &profile_sourced,
                &command_ran,
            ] {
                if marker.exists() {
                    fs::remove_file(marker).expect("clear Zsh side-effect marker");
                }
            }
        };
        let run = |isolated: bool| {
            let mut args = vec!["--shell".to_string(), "zsh".to_string()];
            if isolated {
                args.push("--isolated".to_string());
            }
            args.extend(shell_args.iter().cloned());

            raw_cli_command(binary)
                .arg0("cosh")
                .env("HOME", &home)
                .env("ZDOTDIR", &home)
                .env("ENV", &env_file)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("run interactive Zsh command")
        };

        clear_markers();
        let plain = run(false);
        let plain_stdout = String::from_utf8_lossy(&plain.stdout);
        let plain_stderr = String::from_utf8_lossy(&plain.stderr);
        assert!(
            plain.status.success(),
            "case={case} stdout={plain_stdout} stderr={plain_stderr}"
        );
        assert_eq!(
            fs::read_to_string(&command_ran).expect("plain Zsh command marker"),
            expected_command,
            "case={case} stdout={plain_stdout} stderr={plain_stderr}"
        );
        let expected_startup = match startup {
            Startup::Zshenv => &zshenv_sourced,
            Startup::Env => &env_sourced,
            Startup::Profile => &profile_sourced,
        };
        assert!(
            expected_startup.exists(),
            "plain Zsh missed startup file: {case}"
        );

        clear_markers();
        let isolated = run(true);
        let isolated_stdout = String::from_utf8_lossy(&isolated.stdout);
        let isolated_stderr = String::from_utf8_lossy(&isolated.stderr);
        assert_eq!(
            isolated.status.code(),
            Some(2),
            "case={case} stdout={isolated_stdout} stderr={isolated_stderr}"
        );
        assert!(
            isolated_stderr.contains("isolated Zsh shell arguments are not supported"),
            "case={case} stdout={isolated_stdout} stderr={isolated_stderr}"
        );
        for marker in [
            &zshenv_sourced,
            &env_sourced,
            &profile_sourced,
            &command_ran,
        ] {
            assert!(!marker.exists(), "isolated Zsh side effect: {case}");
        }
    }

    fs::remove_dir_all(home).expect("remove Zsh fail-closed fixture");
}

#[test]
fn raw_cli_cosh_entry_plain_dash_c_preserves_env_and_argv() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let home = temp_shell_home("plain-bash-passthrough");
    let shell = home.join("bash");
    fs::write(
        &shell,
        "#!/bin/sh\nprintf 'BASH_ENV=<%s>\\n' \"${BASH_ENV-__UNSET__}\"\n\
         for arg do printf 'ARG=<%s>\\n' \"$arg\"; done\n",
    )
    .expect("write recording bash fixture");
    let mut permissions = fs::metadata(&shell)
        .expect("recording bash metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shell, permissions).expect("make recording bash executable");

    let bash_env = home.join("plain-bash-env.sh");
    let shell = shell.to_string_lossy().into_owned();
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .env("BASH_ENV", &bash_env)
        .args([
            "--shell",
            shell.as_str(),
            "-c",
            "printf __COMMAND_RAN__",
            "label",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run plain cosh entry on the exec path");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            format!("BASH_ENV=<{}>", bash_env.display()),
            "ARG=<-c>".to_string(),
            "ARG=<printf __COMMAND_RAN__>".to_string(),
            "ARG=<label>".to_string(),
        ],
        "stderr={stderr}"
    );

    fs::remove_dir_all(home).expect("remove plain passthrough fixture");
}

#[test]
fn raw_cli_cosh_entry_isolated_login_rejects_before_profile_and_bash_env() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let home = temp_shell_home("isolated-login");
    let profile_sourced = home.join("profile-sourced");
    let bash_env_sourced = home.join("bash-env-sourced");
    let bash_env = home.join("bash-env.sh");
    fs::write(
        home.join(".bash_profile"),
        format!("printf sourced > '{}'\n", profile_sourced.display()),
    )
    .expect("write login profile fixture");
    fs::write(
        &bash_env,
        format!("printf sourced > '{}'\n", bash_env_sourced.display()),
    )
    .expect("write BASH_ENV fixture");

    let plain = raw_cli_command(binary)
        .arg0("cosh")
        .env("HOME", &home)
        .env("BASH_ENV", &bash_env)
        .args(["--login", "-c", "printf __PLAIN_LOGIN_RAN__"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run plain login command");
    let plain_stdout = String::from_utf8_lossy(&plain.stdout);
    let plain_stderr = String::from_utf8_lossy(&plain.stderr);
    assert!(
        plain.status.success(),
        "stdout={plain_stdout}\nstderr={plain_stderr}"
    );
    assert!(
        profile_sourced.exists(),
        "login control did not source profile"
    );
    assert!(
        bash_env_sourced.exists(),
        "login control did not source BASH_ENV"
    );
    fs::remove_file(&profile_sourced).expect("reset login profile marker");
    fs::remove_file(&bash_env_sourced).expect("reset BASH_ENV marker");

    let output = raw_cli_command(binary)
        .arg0("cosh")
        .env("HOME", &home)
        .env("BASH_ENV", &bash_env)
        .args([
            "--isolated",
            "--login",
            "-c",
            "printf __LOGIN_COMMAND_RAN__",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run isolated login command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}\nstderr={stderr}");
    assert!(stderr.contains("isolated Bash login shells are not supported"));
    assert!(!profile_sourced.exists(), "login profile was sourced");
    assert!(!bash_env_sourced.exists(), "BASH_ENV was sourced");

    fs::remove_dir_all(home).expect("remove isolated login fixture");
}

#[test]
fn raw_cli_cosh_entry_isolated_bash_login_blocks_logout_files() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let home = tempfile::tempdir().expect("create logout fixture");
    fs::write(
        home.path().join(".bash_logout"),
        "printf __LOGOUT_RAN__; exit 73\n",
    )
    .expect("write logout fixture");

    for (argv0, options) in [
        ("cosh", vec!["--login", "-c"]),
        ("cosh", vec!["-login", "-c"]),
        ("cosh", vec!["-l", "-c"]),
        ("cosh", vec!["+l", "-c"]),
        ("cosh", vec!["-lc"]),
        ("cosh", vec!["-cl"]),
        ("cosh", vec!["-c", "-l"]),
        ("cosh", vec!["-c", "-rcfile"]),
        ("cosh", vec!["-norc", "-noprofile", "-l", "-c"]),
        ("cosh", vec!["-posix", "-l", "-c"]),
        ("cosh", vec!["-ol", "pipefail", "-c"]),
        ("cosh", vec!["-O", "extglob", "+lc"]),
        ("cosh", vec!["--rcfile", "ignored", "-ilc"]),
        ("-cosh", vec!["-c"]),
    ] {
        for isolated in [false, true] {
            let mut command = raw_cli_command(binary);
            command.arg0(argv0).env("HOME", home.path());
            if isolated {
                command.arg("--isolated");
            }
            let output = command
                .args(["--noprofile", "--norc"])
                .args(&options)
                .args(["printf __COMMAND_RAN__; exit 7"])
                .stdin(Stdio::null())
                .output()
                .expect("run logout probe");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(if isolated { 2 } else { 73 }),
                "argv0={argv0} options={options:?} isolated={isolated} stderr={stderr}"
            );
            assert_eq!(
                stdout,
                if isolated {
                    ""
                } else {
                    "__COMMAND_RAN____LOGOUT_RAN__"
                },
                "argv0={argv0} options={options:?} isolated={isolated} stderr={stderr}"
            );
            if isolated {
                assert!(stderr.contains("isolated Bash login shells are not supported"));
            }
        }
    }

    // Option values and command arguments must not become login requests.
    for options in [
        vec!["-c"],
        vec!["-noprofile", "-c"],
        vec!["--rcfile", "-l", "-c"],
        vec!["-init-file", "--login", "-c"],
        vec!["-o", "pipefail", "-c"],
        vec!["-O", "login_shell", "-c"],
        vec!["-c", "--"],
    ] {
        let output = raw_cli_command(binary)
            .arg0("cosh")
            .env("HOME", home.path())
            .arg("--isolated")
            .args(&options)
            .args(["printf '%s|%s' \"$0\" \"$1\"; exit 7", "--login", "-lc"])
            .stdin(Stdio::null())
            .output()
            .expect("run non-login control");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(7),
            "options={options:?} stderr={stderr}"
        );
        assert_eq!(
            output.stdout, b"--login|-lc",
            "options={options:?} stderr={stderr}"
        );
    }
}

#[test]
fn raw_cli_cosh_entry_isolated_network_stdin_translates_bash_isolation() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let home = temp_shell_home("isolated-network-stdin");
    let shell = home.join("bash");
    fs::write(
        &shell,
        "#!/bin/sh\nprintf 'BASH_ENV=<%s>\\n' \"${BASH_ENV-__UNSET__}\"\n\
         for arg do printf 'ARG=<%s>\\n' \"$arg\"; done\n",
    )
    .expect("write recording bash fixture");
    let mut permissions = fs::metadata(&shell)
        .expect("recording bash metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shell, permissions).expect("make recording bash executable");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind network stdin fixture");
    let client = TcpStream::connect(listener.local_addr().expect("listener address"))
        .expect("connect network stdin fixture");
    let (server, _) = listener.accept().expect("accept network stdin fixture");
    let server: OwnedFd = server.into();
    let shell = shell.to_string_lossy().into_owned();
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .env("BASH_ENV", home.join("would-source.sh"))
        .args([
            "--shell",
            shell.as_str(),
            "--isolated",
            "-c",
            "printf __COMMAND_RAN__",
        ])
        .stdin(Stdio::from(server))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run isolated command with network stdin");
    drop(client);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            "BASH_ENV=<__UNSET__>",
            "ARG=<--noprofile>",
            "ARG=<--norc>",
            "ARG=<-c>",
            "ARG=<printf __COMMAND_RAN__>",
        ],
        "stderr={stderr}"
    );

    fs::remove_dir_all(home).expect("remove network stdin fixture");
}

#[test]
fn raw_cli_cosh_entry_isolated_dash_c_preserves_command_argv() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .args([
            "--isolated",
            "-c",
            "printf '<%s>|<%s>|<%s>' \"$0\" \"$1\" \"$2\"",
            "label",
            "one",
            "two",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run isolated command argv probe");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(stdout, "<label>|<one>|<two>", "stderr={stderr}");
}

#[test]
fn raw_cli_cosh_entry_isolated_dash_c_preserves_signal_status() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .args(["--isolated", "-c", "kill -TERM $$"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run isolated command terminated by signal");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.signal(),
        Some(15),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_passthrough_preserves_ignored_sigpipe_disposition() {
    // An ignored SIGPIPE inherited by the cosh entry must reach the inner
    // shell (captured before the Rust runtime rewrite, restored in
    // pre_exec); the default disposition must stay default. SIGPIPE is
    // signal 13, so bit 13 of SigIgn is mask 0x1000.
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let probe = "grep SigIgn /proc/self/status";

    let sigign_mask = |ignore_pipe: bool| -> u64 {
        let prefix = if ignore_pipe { "trap '' PIPE; " } else { "" };
        let script = format!("{prefix}exec -a cosh '{binary}' -c '{probe}'");
        let output = raw_cli_command("bash")
            .args(["-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run sigpipe disposition probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
        let hex = stdout
            .split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("no SigIgn value in {stdout:?}"));
        u64::from_str_radix(hex, 16).expect("parse SigIgn mask")
    };

    assert_ne!(
        sigign_mask(true) & 0x1000,
        0,
        "ignored SIGPIPE must be inherited by the inner shell"
    );
    assert_eq!(
        sigign_mask(false) & 0x1000,
        0,
        "default SIGPIPE must stay default in the inner shell"
    );
}

#[test]
fn raw_cli_interactive_dash_c_passthrough_transports_env_ps1() {
    // Value-level prompt contract: env PS1 survives the compiled entry and
    // is visible to an interactive inner bash (non-interactive bash strips
    // it natively on both the oracle and candidate sides).
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg0("cosh")
        .env("PS1", "__COSH_PS1_PROBE__")
        .args([
            "--norc",
            "--noprofile",
            "-i",
            "-c",
            "printf '[%s]' \"$PS1\"",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run interactive dash-c with env PS1");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("[__COSH_PS1_PROBE__]"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

struct InteractivePtyChild(std::process::Child);

impl Drop for InteractivePtyChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            unsafe { nix::libc::kill(-(self.0.id() as i32), nix::libc::SIGKILL) };
            self.0.wait().expect("reap explicit interactive shell");
        }
    }
}

fn interactive_pty_probe(
    program: &str,
    args: &[&str],
    home: &Path,
    ready: &str,
    done: &str,
) -> String {
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    use nix::libc;

    let pty = nix::pty::openpty(None, None).expect("open explicit interactive PTY");
    let mut master = File::from(pty.master);
    let slave = File::from(pty.slave);
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0, "read PTY flags");
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0,
        "make PTY reads nonblocking"
    );
    let mut command = Command::new(program);
    command
        .arg0("cosh")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("PS1", "__ENV_READY__ ")
        .env("INPUTRC", "/dev/null")
        .env("HISTFILE", "/dev/null")
        .env("TERM", "xterm-256color")
        .env("COSH_SHELL_BOOTSTRAP_PATH", "0")
        .env("COSH_SHELL_STARTUP_BANNER", "0")
        .env("COSH_SHELL_HEALTH_SCAN", "disabled")
        .env("COSH_RECOMMENDATIONS_ENABLED", "0")
        .env("LC_ALL", "C")
        .env("COSH_SHELL_DEFAULT_SHELL", "bash")
        .current_dir(home)
        .stdin(Stdio::from(slave.try_clone().expect("clone PTY stdin")))
        .stdout(Stdio::from(slave.try_clone().expect("clone PTY stdout")))
        .stderr(Stdio::from(slave));
    command.args(args);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = InteractivePtyChild(command.spawn().expect("spawn interactive shell"));
    let mut output = Vec::new();
    let mut wait_for = |master: &mut File, marker: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !String::from_utf8_lossy(&output).contains(marker) {
            assert!(
                Instant::now() < deadline,
                "{program}, args={args:?}: timed out before {marker:?}: {output:?}"
            );
            let mut bytes = [0; 4096];
            match master.read(&mut bytes) {
                Ok(0) => panic!("PTY closed before {marker:?}: {output:?}"),
                Ok(count) => output.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("PTY read before {marker:?}: {error}; {output:?}"),
            }
        }
    };
    wait_for(&mut master, ready);
    // The command is delivered only after Bash actually renders its first PS1.
    master
        .write_all(b"printf '__DONE__<%s>\\n' \"${COSH_RC_PROBE-unset}\"\nexit\n")
        .expect("write prompt-gated probe");
    wait_for(&mut master, done);
    let status = child
        .0
        .wait_timeout(Duration::from_secs(10))
        .expect("wait interactive shell")
        .expect("interactive shell exit timeout");
    assert!(status.success(), "{program}, args={args:?}: {status:?}");
    String::from_utf8_lossy(&output).into_owned()
}

#[test]
fn raw_cli_explicit_interactive_norc_preserves_prompt_readiness() {
    for program in ["bash", env!("CARGO_BIN_EXE_cosh-shell")] {
        for norc in [false, true] {
            let home = tempfile::Builder::new()
                .prefix("cosh-explicit-interactive-")
                .tempdir()
                .expect("isolated interactive HOME");
            fs::write(
                home.path().join(".bashrc"),
                "printf '__RC_READ__\\n'\nCOSH_RC_PROBE=loaded\nPS1='__RC_READY__ '\n",
            )
            .expect("write startup sentinel");
            let (args, ready, done): (&[&str], _, _) = if norc {
                (&["--norc", "-i"], "__ENV_READY__ ", "__DONE__<unset>")
            } else {
                (&["-i"], "__RC_READY__ ", "__DONE__<loaded>")
            };
            let output = interactive_pty_probe(program, args, home.path(), ready, done);
            assert_eq!(output.contains("__RC_READ__"), !norc, "{output}");
            assert_eq!(output.contains("__RC_READY__"), !norc, "{output}");
            assert_eq!(output.contains("__ENV_READY__"), norc, "{output}");
        }
    }
}

#[test]
fn raw_cli_cosh_entry_default_enhanced_reaches_user_prompt() {
    let home = tempfile::Builder::new()
        .prefix("cosh-default-interactive-")
        .tempdir()
        .expect("isolated default interactive HOME");
    write_cosh_config(home.path(), "[shell]\nadapter_default = 'fake'\n");
    fs::write(
        home.path().join(".bashrc"),
        "COSH_RC_PROBE=loaded\nPS1='__RC_READY__ '\n",
    )
    .expect("write user prompt");
    let output = interactive_pty_probe(
        env!("CARGO_BIN_EXE_cosh-shell"),
        &[],
        home.path(),
        "__RC_READY__ ",
        "__DONE__<loaded>",
    );
    let visible = strip_ansi_escape(&output);
    assert!(
        visible.replace('\r', "").contains("◇ \n__RC_READY__ "),
        "{output}"
    );
    assert_ordered(&visible, &["__RC_READY__ ", "__DONE__<loaded>"]);
}
