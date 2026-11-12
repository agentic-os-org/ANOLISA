use std::net::{TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_remote_bashrc_startup_counts_match_native_bash() {
    let counts = |binary: &str, args: &[&str], socket_stdin: bool| {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join(".bashrc"),
            "printf 'hit\\n' >> \"$HOME/bashrc-hits\"\n",
        )
        .unwrap();
        ["true", ". ~/.bashrc"].map(|script| {
            let (input, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let stdin = if socket_stdin {
                Stdio::from(OwnedFd::from(input))
            } else {
                Stdio::null()
            };
            let output = Command::new(binary)
                .args(args)
                .args(["-c", script])
                .env_clear()
                .env("HOME", home.path())
                .env("PATH", "/usr/bin:/bin")
                .env("SSH_CLIENT", "127.0.0.1 12345 22")
                .env("SHLVL", "0")
                .stdin(stdin)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            fs::read_to_string(home.path().join("bashrc-hits"))
                .unwrap_or_default()
                .lines()
                .count()
        })
    };
    for socket_stdin in [false, true] {
        let native = counts("/bin/bash", &[], socket_stdin);
        assert_eq!(native[1], native[0] * 2 + 1);
        assert_eq!(
            counts(
                env!("CARGO_BIN_EXE_cosh-shell"),
                &["--shell", "bash"],
                socket_stdin
            ),
            native
        );
    }
}

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
fn raw_cli_primary_passthrough_preserves_ignored_sigchld_disposition() {
    let probe = |binary: &str| {
        let mut command = raw_cli_command(binary);
        command
            .arg0("cosh")
            .args(["-c", "printf 'OK\\n'; exec /bin/cat /proc/self/status"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                let mut action: nix::libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = nix::libc::SIG_IGN;
                nix::libc::sigemptyset(&mut action.sa_mask);
                if nix::libc::sigaction(nix::libc::SIGCHLD, &action, std::ptr::null_mut()) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().expect("run inherited SIGCHLD probe");
        assert!(output.status.success(), "{binary}: {output:?}");
        assert!(output.stderr.is_empty(), "{binary}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.starts_with("OK\n"), "{binary}: {stdout}");
        let mask = stdout
            .lines()
            .find_map(|line| line.strip_prefix("SigIgn:"))
            .expect("read inherited signal mask");
        u64::from_str_radix(mask.trim(), 16).expect("parse inherited signal mask")
    };
    let sigchld = 1u64 << (nix::libc::SIGCHLD - 1);
    let native = probe("/bin/bash") & sigchld;
    assert_ne!(
        native, 0,
        "native Bash must retain inherited SIGCHLD ignore"
    );
    assert_eq!(probe(env!("CARGO_BIN_EXE_cosh-shell")) & sigchld, native);
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
    interactive_pty_probe_with_env(program, args, home, ready, done, &[])
}

pub(super) fn interactive_pty_probe_with_env(
    program: &str,
    args: &[&str],
    home: &Path,
    ready: &str,
    done: &str,
    extra_envs: &[(&str, &str)],
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
        .envs(extra_envs.iter().copied())
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
        visible.replace('\r', "").contains("__RC_READY__ "),
        "{output}"
    );
    assert!(
        !visible.contains("◇ ") && !visible.contains("◌ "),
        "default Enhanced must not emit status symbol lines: {output}"
    );
    assert_ordered(&visible, &["__RC_READY__ ", "__DONE__<loaded>"]);
}

#[test]
fn raw_cli_raw_launch_consumed_shell_values_preserve_login_argv0() {
    // Isolated Enhanced launches expose parsed login intent through COSH_LOGIN_SHELL.
    for argv0 in ["cosh", "cosh-shell", "-cosh", "-cosh-shell"] {
        for value in ["-l", "--login"] {
            let mut command = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"));
            command
                .arg0(argv0)
                .args(["raw", "fake", "--shell", "bash", "--shell", value])
                .env("COSH_SHELL_ISOLATED", "1")
                .env_remove("COSH_LOGIN_SHELL")
                .env("COSH_SHELL_STARTUP_BANNER", "0")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().expect("spawn raw login ownership probe");
            child
                .stdin
                .take()
                .expect("probe stdin")
                .write_all(b"printf '__LOGIN_MARKER_%s__\\n' \"${COSH_LOGIN_SHELL-unset}\"\nexit\n")
                .expect("write raw login ownership probe");
            let output = wait_for_raw_cli_output(child);
            let text = String::from_utf8_lossy(&output.stdout);
            let expected = if argv0.starts_with('-') {
                "__LOGIN_MARKER_1__"
            } else {
                "__LOGIN_MARKER_unset__"
            };
            assert!(
                output.status.success(),
                "argv0={argv0}, value={value}, output={output:?}"
            );
            assert!(
                text.contains(expected),
                "argv0={argv0}, value={value}, output={output:?}"
            );
        }
    }
}

#[test]
fn raw_cli_ask_rejects_non_interactive_before_runtime_initialization() {
    for argv0 in ["cosh", "cosh-shell"] {
        for args in [
            vec!["--ask", "question"],
            vec!["raw", "fake", "--ask", "question"],
        ] {
            let home = tempfile::tempdir().expect("ask usage fixture home");
            let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
                .arg0(argv0)
                .args(args)
                .env("HOME", home.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("run non-interactive ask");
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("--ask requires an interactive terminal"),
                "{output:?}"
            );
            assert!(!home.path().join(".copilot-shell/logs").exists());
        }
    }
}

#[test]
fn raw_cli_ask_after_shell_boundary_remains_shell_argv() {
    let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
        .arg0("cosh")
        .args([
            "-c",
            "printf '%s|%s' \"$1\" \"$2\"",
            "name",
            "--ask",
            "--login",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run ask-shaped shell arguments");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"--ask|--login");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_ask_uses_enhanced_for_native_config_without_executing_or_recording_question() {
    for raw in [false, true] {
        let home = tempfile::Builder::new()
            .prefix("ask-fixtures-cosh-initial-question-")
            .tempdir()
            .expect("ask fixture home");
        let config = "[shell]\nadapter_default = 'fake'\nintegration = 'native'\n";
        write_cosh_config(home.path(), config);
        fs::write(home.path().join(".bashrc"), "COSH_RC_PROBE=loaded\nPS1='__ASK_PROMPT__ '\nPROMPT_COMMAND='history -w \"$HOME/history.snapshot\"'\n").expect("write ask shell fixture");
        let sentinel = home.path().join("initial-prompt-must-not-execute");
        let question = "touch initial-prompt-must-not-execute";
        let args = if raw {
            vec!["raw", "fake", "--ask", question]
        } else {
            vec!["--ask", question]
        };
        let output = interactive_pty_probe_with_env(
            env!("CARGO_BIN_EXE_cosh-shell"),
            &args,
            home.path(),
            "Received shell prompt request:",
            "__DONE__<loaded>",
            &[
                ("COSH_SHELL_STARTUP_BANNER", "1"),
                ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
                ("COSH_RECOMMENDATIONS_ENABLED", "1"),
                ("COSH_SHELL_LANG", "en-US"),
            ],
        );
        let visible = strip_ansi_escape(&output).replace('\r', "");
        assert!(visible.contains("Cosh AI Shell"), "{output}");
        assert!(
            visible.contains(&format!("__ASK_PROMPT__ {question}")),
            "{output}"
        );
        assert!(
            visible.contains("Run commands or ask AI questions."),
            "{output}"
        );
        assert!(
            !visible
                .to_lowercase()
                .contains("read-only tools auto-approved"),
            "{output}"
        );
        assert!(!visible.contains("████"), "{output}");
        assert!(
            !visible.contains("Prompt recommendations are on"),
            "{output}"
        );
        assert!(!visible.contains("Memory:"), "{output}");
        assert!(
            !sentinel.exists(),
            "initial question executed as shell code: {output}"
        );
        let history = fs::read_to_string(home.path().join("history.snapshot"))
            .expect("read native shell history snapshot");
        assert!(
            !history.contains(question),
            "initial question entered shell history: {history}"
        );
        assert_eq!(
            fs::read_to_string(home.path().join(".copilot-shell/config.toml")).unwrap(),
            config
        );
    }
}

#[test]
fn raw_cli_guide_owned_namespace_updates_current_user_without_tui() {
    for argv0 in ["cosh", "cosh-shell"] {
        let home = tempfile::tempdir().expect("guide fixture home");
        #[cfg(target_os = "linux")]
        let runtime = tempfile::tempdir_in("/dev/shm").expect("local guide fixture runtime");
        #[cfg(not(target_os = "linux"))]
        let runtime = tempfile::tempdir().expect("guide fixture runtime");
        for directory in [home.path(), runtime.path()] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .expect("make guide fixture directory private");
        }
        let home_path = home.path().canonicalize().expect("canonical guide home");
        let runtime_path = runtime
            .path()
            .canonicalize()
            .expect("canonical guide runtime");
        for (leaf, enabled) in [
            ("off", false),
            ("status", false),
            ("on", true),
            ("status", true),
        ] {
            let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
                .arg0(argv0)
                .args(["guide", leaf])
                .env("HOME", &home_path)
                .env("XDG_RUNTIME_DIR", &runtime_path)
                .env("SUDO_USER", "guide-test-must-not-target-this-user")
                .env("COSH_SHELL_LANG", "en-US")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("run owned guide command");
            assert!(
                output.status.success(),
                "argv0={argv0}, leaf={leaf}, {output:?}"
            );
            assert!(output.stderr.is_empty(), "{output:?}");
            assert!(!output.stdout.is_empty());
            let preference: serde_json::Value = serde_json::from_slice(
                &fs::read(
                    home.path()
                        .join(".copilot-shell/cosh/guide/preferences.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(preference["guide_enabled"], enabled);
            assert!(!home.path().join(".copilot-shell/logs").exists());
            assert!(!String::from_utf8_lossy(&output.stdout).contains("Cosh AI Shell"));
        }
        let invalid = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
            .arg0(argv0)
            .args(["guide", "invalid"])
            .env("HOME", &home_path)
            .env("XDG_RUNTIME_DIR", &runtime_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run invalid owned guide command");
        assert_eq!(invalid.status.code(), Some(2), "{invalid:?}");
        assert!(invalid.stdout.is_empty());
        assert!(!invalid.stderr.is_empty());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_ask_preserves_an_empty_native_prompt_and_keeps_shell() {
    let home = tempfile::Builder::new()
        .prefix("cosh-ask-no-prompt-")
        .tempdir()
        .unwrap();
    write_cosh_config(
        home.path(),
        "[shell]\nadapter_default='fake'\nintegration='native'\n",
    );
    fs::write(
        home.path().join(".bashrc"),
        "PS1=''\nCOSH_RC_PROBE=loaded\n",
    )
    .unwrap();
    let output = interactive_pty_probe_with_env(
        env!("CARGO_BIN_EXE_cosh-shell"),
        &["--ask", "original question"],
        home.path(),
        "Received shell prompt request:",
        "__DONE__<loaded>",
        &[("COSH_SHELL_LANG", "en-US")],
    );
    assert!(
        output.contains("Received shell prompt request:"),
        "{output}"
    );
    assert!(
        !output.contains("initial question was not submitted"),
        "{output}"
    );
    assert!(!output.contains("◆"), "{output}");
}

#[test]
fn raw_cli_private_health_worker_dispatches_before_configuration_without_tui() {
    let flag = "--__cosh-health-worker-v1";
    for argv0 in ["cosh", "cosh-shell"] {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".copilot-shell")).unwrap();
        fs::write(
            home.path().join(".copilot-shell/config.toml"),
            "invalid = [",
        )
        .unwrap();
        for extra in [false, true] {
            let mut command = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"));
            command.arg0(argv0).arg(flag);
            if extra {
                command.arg("unexpected");
            }
            let output = command
                .env("HOME", home.path())
                .env("COSH_SHELL_DEFAULT_SHELL", "/missing-health-worker-shell")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{argv0}: {output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            if extra {
                assert!(String::from_utf8_lossy(&output.stderr)
                    .contains("private health worker does not accept arguments"));
            } else {
                assert!(
                    output.stderr.is_empty(),
                    "unisolated worker should fail quietly: {output:?}"
                );
            }
            assert!(!home.path().join(".copilot-shell/logs").exists());
        }
    }
}

#[test]
fn raw_cli_private_health_worker_flag_in_shell_owned_tail_stays_shell_data() {
    let flag = "--__cosh-health-worker-v1";
    let home = tempfile::tempdir().unwrap();
    let script = home.path().join("shell-owned.sh");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '__WORKER_TAIL__%s__\\n' \"$1\"\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    for argv0 in ["cosh", "cosh-shell"] {
        for args in [
            vec!["-c", "printf '__WORKER_TAIL__%s__\\n' \"$1\"", "_", flag],
            vec![script.to_str().unwrap(), flag],
            vec!["--", script.to_str().unwrap(), flag],
            vec![
                "raw",
                "fake",
                "-c",
                "printf '__WORKER_TAIL__%s__\\n' \"$1\"",
                "_",
                flag,
            ],
        ] {
            let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
                .arg0(argv0)
                .args(&args)
                .env("HOME", home.path())
                .env("COSH_SHELL_DEFAULT_SHELL", "/bin/bash")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            assert!(output.status.success(), "{argv0} {args:?}: {output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                format!("__WORKER_TAIL__{flag}__")
            );
            assert!(output.stderr.is_empty(), "{output:?}");
        }
    }
}

#[test]
fn raw_cli_login_owned_namespace_rejects_bad_usage_before_configuration_or_tui() {
    for argv0 in ["cosh", "cosh-shell"] {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".copilot-shell")).unwrap();
        fs::write(
            home.path().join(".copilot-shell/config.toml"),
            "invalid = [",
        )
        .unwrap();
        for args in [
            vec!["login"],
            vec!["login", "invalid"],
            vec!["login", "status", "--yes"],
            vec!["login", "set", "--user"],
        ] {
            let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
                .arg0(argv0)
                .args(&args)
                .env("HOME", home.path())
                .env("COSH_SHELL_DEFAULT_SHELL", "/missing-login-shell")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(2),
                "{argv0} {args:?}: {output:?}"
            );
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("usage: cosh login"));
            assert!(!home.path().join(".copilot-shell/logs").exists());
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_login_status_uses_effective_uid_and_ignores_shell_and_sudo_user_environment() {
    let uid = unsafe { nix::libc::geteuid() };
    let passwd = fs::read_to_string("/etc/passwd").unwrap();
    let username = passwd
        .lines()
        .find_map(|line| {
            let fields = line.split(':').collect::<Vec<_>>();
            (fields.len() == 7 && fields[2].parse::<u32>().ok() == Some(uid))
                .then(|| fields[0].to_string())
        })
        .unwrap();
    let release = fs::read_to_string("/etc/os-release").unwrap();
    let supported = release
        .lines()
        .filter_map(|line| line.split_once('='))
        .any(|(key, value)| key == "ID" && value.trim_matches('"') == "alinux")
        && release
            .lines()
            .filter_map(|line| line.split_once('='))
            .any(|(key, value)| {
                let value = value.trim_matches('"');
                key == "VERSION_ID" && (value == "4" || value.starts_with("4."))
            });
    for argv0 in ["cosh", "cosh-shell"] {
        let home = tempfile::tempdir().unwrap();
        let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
            .arg0(argv0)
            .args(["login", "status"])
            .env("HOME", home.path())
            .env("SUDO_USER", "login-test-not-the-target")
            .env("SHELL", "/untrusted/test/shell")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        if supported {
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(
                text.contains(&format!("User: {username} (UID {uid})")),
                "{output:?}"
            );
            assert!(!text.contains("/untrusted/test/shell"));
            assert!(output.stderr.is_empty());
        } else {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("Alibaba Cloud Linux 4"));
        }
        assert!(!home.path().join(".copilot-shell/logs").exists());
    }
}

#[test]
fn raw_cli_login_messages_follow_environment_language_without_opening_config() {
    for argv0 in ["cosh", "cosh-shell"] {
        for (language, locale, chinese) in [
            (Some("zh-CN"), "C.UTF-8", true),
            (Some("en-US"), "zh_CN.UTF-8", false),
            (Some("auto"), "zh_CN.UTF-8", true),
            (Some("invalid"), "zh_CN.UTF-8", true),
            (None, "C.UTF-8", false),
        ] {
            let home = tempfile::tempdir().unwrap();
            fs::create_dir(home.path().join(".copilot-shell")).unwrap();
            nix::unistd::mkfifo(
                &home.path().join(".copilot-shell/config.toml"),
                nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
            )
            .unwrap();
            for action in ["invalid", "status"] {
                let mut command = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"));
                command
                    .arg0(argv0)
                    .args(["login", action])
                    .env("HOME", home.path())
                    .env("LC_ALL", locale)
                    .env("LANG", "C")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                if let Some(language) = language {
                    command.env("COSH_SHELL_LANG", language);
                } else {
                    command.env_remove("COSH_SHELL_LANG");
                }
                let output = wait_for_raw_cli_output(command.spawn().unwrap());
                let text = String::from_utf8_lossy(&output.stdout);
                let error = String::from_utf8_lossy(&output.stderr);
                if action == "invalid" {
                    assert_eq!(output.status.code(), Some(2), "{output:?}");
                    assert!(text.is_empty(), "{output:?}");
                    assert!(
                        error.contains(if chinese {
                            "用法：cosh login"
                        } else {
                            "usage: cosh login"
                        }),
                        "{language:?}/{locale}: {output:?}"
                    );
                } else if !text.is_empty() {
                    assert!(error.is_empty(), "{output:?}");
                    assert!(
                        text.contains(if chinese {
                            "当前登录 Shell："
                        } else {
                            "Current login shell:"
                        }),
                        "{language:?}/{locale}: {output:?}"
                    );
                } else {
                    assert_eq!(output.status.code(), Some(1), "{output:?}");
                    assert!(
                        error.contains(if chinese {
                            "操作未完成"
                        } else {
                            "cosh login:"
                        }),
                        "{language:?}/{locale}: {output:?}"
                    );
                }
                assert!(!home.path().join(".copilot-shell/logs").exists());
            }
        }
    }
}

#[test]
fn raw_cli_shell_integration_private_worker_rejects_unsupervised_invocation_without_io() {
    for argv0 in ["cosh", "cosh-shell"] {
        let home = tempfile::tempdir().unwrap();
        let child = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
            .arg0(argv0)
            .args(["shell-integration", "--__compat-worker-v2"])
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = wait_for_raw_cli_output(child);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
        assert!(fs::read_dir(home.path()).unwrap().next().is_none());
    }
}

#[test]
fn raw_cli_shell_integration_early_dispatch_uses_only_the_nul_protocol() {
    for argv0 in ["cosh", "cosh-shell"] {
        for operation in ["compat", "advise"] {
            let home = tempfile::tempdir().unwrap();
            fs::create_dir(home.path().join(".copilot-shell")).unwrap();
            fs::write(
                home.path().join(".copilot-shell/config.toml"),
                "invalid = [",
            )
            .unwrap();
            let budget = if operation == "compat" { "100" } else { "50" };
            #[cfg(target_os = "linux")]
            let absolute = {
                let mut now: nix::libc::timespec = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_BOOTTIME, &mut now) },
                    0
                );
                (now.tv_sec as u64 * 1_000_000
                    + now.tv_nsec as u64 / 1000
                    + budget.parse::<u64>().unwrap() * 1000
                    - 1000)
                    .to_string()
            };
            #[cfg(not(target_os = "linux"))]
            let absolute = "1".to_string();
            let mut fields = vec![
                "2",
                "external-bash",
                if operation == "compat" {
                    "registration"
                } else {
                    "activation"
                },
                if operation == "compat" {
                    ""
                } else {
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                },
                absolute.as_str(),
                budget,
            ];
            if operation == "advise" {
                fields.extend([
                    "cargo build",
                    "1",
                    "/tmp",
                    "0",
                    "observed_command_fragment",
                    "external_unshadowed",
                    "host",
                    "session",
                    "0",
                ]);
            }
            let mut input = fields.join("\0").into_bytes();
            input.push(0);
            let mut child = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
                .arg0(argv0)
                .args(["shell-integration", operation])
                .env("HOME", home.path())
                .env(
                    "COSH_SHELL_DEFAULT_SHELL",
                    "/missing-shell-integration-shell",
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&input).unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{argv0}/{operation}: {output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            let frame = output
                .stdout
                .strip_suffix(&[0])
                .expect("NUL-terminated response");
            let fields = frame.split(|byte| *byte == 0).collect::<Vec<_>>();
            assert_eq!(fields.len(), 8, "{output:?}");
            assert_eq!(fields[0], b"2");
            assert_eq!(fields[1], b"DISABLE");
            assert!(
                fields[2] == b"incompatible_runtime"
                    || fields[2] == b"deadline_exceeded"
                    || fields[2] == b"verification_unavailable",
                "{output:?}"
            );
            assert!(!home.path().join(".copilot-shell/logs").exists());
        }
    }
}

#[test]
fn raw_cli_shell_integration_rejects_unknown_or_extra_leaf_without_starting_runtime() {
    for argv0 in ["cosh", "cosh-shell"] {
        for args in [
            vec!["shell-integration"],
            vec!["shell-integration", "unknown"],
            vec!["shell-integration", "compat", "extra"],
        ] {
            let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
                .arg0(argv0)
                .args(&args)
                .env(
                    "COSH_SHELL_DEFAULT_SHELL",
                    "/missing-shell-integration-shell",
                )
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
        }
    }
}

#[test]
fn raw_cli_health_banner_early_dispatch_is_quiet_and_management_is_independent() {
    for argv0 in ["cosh", "cosh-shell"] {
        let home = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        for path in [home.path(), runtime.path()] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let home_path = home.path().canonicalize().unwrap();
        let runtime_path = runtime.path().canonicalize().unwrap();
        let invoke = |args: &[&str]| {
            raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
                .arg0(argv0)
                .args(args)
                .env("HOME", &home_path)
                .env("XDG_RUNTIME_DIR", &runtime_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap()
        };
        let quiet = invoke(&["health", "banner"]);
        assert!(quiet.status.success(), "{quiet:?}");
        assert!(
            quiet.stdout.is_empty() && quiet.stderr.is_empty(),
            "{quiet:?}"
        );
        assert!(!home_path.join(".copilot-shell").exists());
        for (leaf, enabled) in [
            ("off", false),
            ("status", false),
            ("on", true),
            ("status", true),
        ] {
            let output = invoke(&["health", "banner", leaf]);
            assert!(output.status.success(), "{output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            let value: serde_json::Value = serde_json::from_slice(
                &fs::read(home_path.join(".copilot-shell/cosh/guide/preferences.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(value["banner_enabled"], enabled);
            assert_eq!(value["guide_enabled"], true);
            assert!(!home_path.join(".copilot-shell/logs").exists());
            assert!(!home_path.join(".copilot-shell/config.toml").exists());
        }
        for args in [
            vec!["health"],
            vec!["health", "invalid"],
            vec!["health", "banner", "invalid"],
        ] {
            assert_eq!(invoke(&args).status.code(), Some(2));
        }
    }
}

#[test]
fn review_regression_health_banner_chinese_usage_and_preference_error() {
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join(".copilot-shell"), "blocked").unwrap();
    for (action, code, expected) in [("invalid", 2, "用法"), ("status", 1, "无法读取或保存")]
    {
        let error_path = home.path().join(format!("{action}.stderr"));
        let error_file = fs::File::create(&error_path).unwrap();
        let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
            .args(["health", "banner", action])
            .env("HOME", home.path())
            .env("COSH_SHELL_LANG", "zh-CN")
            .stderr(Stdio::from(error_file))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        let error = fs::read_to_string(error_path).unwrap();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn raw_cli_login_invitation_worker_is_private_early_and_rejects_extra_arguments() {
    for argv0 in ["cosh", "cosh-shell"] {
        let home = tempfile::tempdir().unwrap();
        for extra in [false, true] {
            let mut command = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"));
            command
                .arg0(argv0)
                .arg("--__cosh-login-invitation-worker-v1");
            if extra {
                command.arg("extra");
            }
            let output = command
                .env("HOME", home.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(if extra { 2 } else { 1 }),
                "{output:?}"
            );
            assert!(output.stdout.is_empty(), "{output:?}");
            assert!(!home.path().join(".copilot-shell").exists());
        }
    }
}

#[test]
fn raw_cli_login_private_management_handlers_dispatch_without_startup_state() {
    for argv0 in ["cosh", "cosh-shell"] {
        let home = tempfile::tempdir().unwrap();
        for (args, expected) in [
            (vec!["--__cosh-login-shell-plan-v1"], 1),
            (vec!["--__cosh-login-shell-plan-v1", "extra"], 2),
            (vec!["--__cosh-login-editor-v1"], 2),
            (vec!["--__cosh-login-lifecycle-v1"], 2),
            (vec!["--__cosh-login-lifecycle-v1", "invalid"], 2),
        ] {
            let output = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"))
                .arg0(argv0)
                .args(&args)
                .env("HOME", home.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(expected), "{args:?}: {output:?}");
            assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
            assert!(!home.path().join(".copilot-shell").exists());
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_ask_explicit_request_keeps_later_ai_disabled_and_shell_available() {
    let home = tempfile::tempdir().unwrap();
    write_cosh_config(
        home.path(),
        "[shell]\nadapter_default='fake'\nintegration='native'\n",
    );
    fs::write(
        home.path().join(".bashrc"),
        "COSH_RC_PROBE=loaded\nPS1='explicit$ '\n",
    )
    .unwrap();
    let sentinel = home.path().join("must-not-execute");
    let question = format!("touch '{}'", sentinel.display());
    let output = interactive_pty_probe_with_env(
        env!("CARGO_BIN_EXE_cosh-shell"),
        &["--ask", &question],
        home.path(),
        "Received shell prompt request:",
        "__DONE__<loaded>",
        &[("COSH_SHELL_AI", "off"), ("COSH_SHELL_LANG", "en-US")],
    );
    assert!(
        output.contains("Received shell prompt request:"),
        "{output}"
    );
    assert!(
        output.contains("Only this --ask request uses AI"),
        "{output}"
    );
    assert!(
        output.contains("remains off for subsequent input"),
        "{output}"
    );
    assert!(!sentinel.exists(), "{output}");
}
