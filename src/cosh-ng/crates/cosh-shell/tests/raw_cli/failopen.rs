//! #3389 fresh-login fail-open regressions.
//!
//! A cosh that is the machine's login shell must never strand the user
//! outside the machine when the *first* interactive session cannot start:
//! pre-ready failures (misconfigured `shell.integration`, host/relay errors,
//! render panics before the first prompt) hand the process over to a clean
//! native `bash -l`, while post-ready failures keep reporting the error
//! because effects may already have been dispatched.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use super::*;
use wait_timeout::ChildExt;

/// Run `cosh-shell raw <adapter>` with the given args/env and scripted
/// stdin, returning (status, stdout+stderr) without asserting the exit
/// code — the post-ready case must observe a failure exit.
///
/// The three tests share one mutex so they never overlap with each other;
/// each spawns exactly one binary, matching the shared-gate budget of the
/// rest of the raw_cli suite.
fn run_failopen_cli(
    extra_args: &[&str],
    envs: &[(&str, &str)],
    input: &[u8],
) -> (i32, String, String) {
    static SEQUENTIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _sequential = SEQUENTIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let mut command = raw_cli_command(binary);
    command
        .args(["raw", "fake"])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    command.process_group(0);
    let mut child = command.spawn().expect("spawn cosh-shell raw");
    // Keep stdin open while waiting: an early EOF could end the session
    // before the first prompt, which would race the fault injection.
    let mut stdin_handle = child.stdin.take().expect("child stdin");
    if !input.is_empty() {
        stdin_handle.write_all(input).expect("write scripted input");
    }
    let output = match child.wait_timeout(Duration::from_secs(30)) {
        Ok(Some(output)) => output,
        Ok(None) => {
            let _ = child.kill();
            panic!("fail-open raw CLI timed out");
        }
        Err(error) => panic!("wait fail-open raw CLI: {error}"),
    };
    drop(stdin_handle);
    let mut stdout_buf = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_end(&mut stdout_buf)
        .expect("read stdout");
    let mut stderr_buf = Vec::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_end(&mut stderr_buf)
        .expect("read stderr");
    (
        output.code().unwrap_or(-1),
        String::from_utf8_lossy(&stdout_buf).into_owned(),
        String::from_utf8_lossy(&stderr_buf).into_owned(),
    )
}

/// Temporary HOME whose profile files make a native `bash -l` deterministic.
fn failopen_shell_home(label: &str, prompt: &str) -> (PathBuf, String) {
    let home = temp_shell_home(label);
    fs::write(home.join(".bash_profile"), format!("PS1='{prompt}'\n")).unwrap();
    let home_str = home.to_string_lossy().to_string();
    (home, home_str)
}

#[test]
fn fresh_login_invalid_integration_falls_back_to_native_bash() {
    let (_home, home_str) = failopen_shell_home("failopen-integration", "fallback$ ");
    let (code, stdout, stderr) = run_failopen_cli(
        &["--shell", "bash", "--login"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "bogus"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        b"echo FALLOPEN_INTEGRATION_MARKER\nexit\n",
    );
    let visible = strip_ansi_escape(&stdout).replace('\r', "");
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stderr.contains("invalid shell integration"),
        "the configuration error must still be reported: {stderr}"
    );
    assert!(
        stderr.contains("falling back to a native login shell"),
        "the fallback must be disclosed: {stderr}"
    );
    assert!(
        visible.contains("FALLOPEN_INTEGRATION_MARKER"),
        "the native login shell must execute the session input: {stdout}"
    );
    let _ = fs::remove_dir_all(_home);
}

#[test]
fn fresh_login_pre_ready_panic_falls_back_to_native_bash() {
    let (_home, home_str) = failopen_shell_home("failopen-pre-ready", "fallback$ ");
    let (code, stdout, stderr) = run_failopen_cli(
        &["--shell", "bash", "--login"],
        &[
            ("HOME", &home_str),
            ("COSH_FAILOPEN_TEST_PANIC", "pre-ready"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        b"echo FALLOPEN_PRE_READY_MARKER\nexit\n",
    );
    let visible = strip_ansi_escape(&stdout).replace('\r', "");
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stderr.contains("runtime failed before the first prompt"),
        "the pre-ready boundary must be disclosed: {stderr}"
    );
    assert!(
        visible.contains("FALLOPEN_PRE_READY_MARKER"),
        "the native login shell must execute the session input: {stdout}"
    );
    let _ = fs::remove_dir_all(_home);
}

#[test]
fn fresh_login_post_ready_panic_does_not_fall_back() {
    let (_home, home_str) = failopen_shell_home("failopen-post-ready", "cosh$ ");
    // No session input: after the first prompt a second shell could
    // duplicate dispatched effects, so the failure must be reported
    // instead of a fallback (which would exit 0 on closed stdin).
    let (code, stdout, stderr) = run_failopen_cli(
        &["--shell", "bash", "--login"],
        &[
            ("HOME", &home_str),
            ("COSH_FAILOPEN_TEST_PANIC", "post-ready"),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        b"",
    );
    let visible = strip_ansi_escape(&stdout).replace('\r', "");
    assert_eq!(code, 1, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stderr.contains("runtime panicked after session start"),
        "the post-ready refusal must be disclosed: {stderr}"
    );
    assert!(
        !stderr.contains("falling back to a native login shell"),
        "post-ready failures must not fall back: {stderr}"
    );
    assert!(
        !visible.contains("fallback$ "),
        "no native login shell may be started after the first prompt: {stdout}"
    );
    let _ = fs::remove_dir_all(_home);
}

#[test]
fn fresh_login_post_ready_relay_err_does_not_fall_back() {
    let (_home, home_str) = failopen_shell_home("failopen-post-ready-err", "cosh$ ");
    // A relay I/O `Err` that arrives after `ShellReady` follows the
    // post-ready panic contract: effects may already have been dispatched,
    // so the failure is reported with exit 1 and no second shell starts.
    let (code, stdout, stderr) = run_failopen_cli(
        &["--shell", "bash", "--login"],
        &[
            ("HOME", &home_str),
            ("COSH_FAILOPEN_TEST_PANIC", "post-ready-err"),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        b"",
    );
    let visible = strip_ansi_escape(&stdout).replace('\r', "");
    assert_eq!(code, 1, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stderr.contains("raw shell failed"),
        "the relay error must still be reported: {stderr}"
    );
    assert!(
        stderr.contains("raw shell failed after session start"),
        "the post-ready refusal must be disclosed: {stderr}"
    );
    assert!(
        !stderr.contains("falling back to a native login shell"),
        "post-ready relay errors must not fall back: {stderr}"
    );
    assert!(
        !visible.contains("fallback$ "),
        "no native login shell may be started after the first prompt: {stdout}"
    );
    let _ = fs::remove_dir_all(_home);
}

#[test]
fn unsupported_shell_with_invalid_integration_keeps_exit_2() {
    let (_home, home_str) = failopen_shell_home("failopen-shell-arg", "fallback$ ");
    // Explicit CLI misuse must not be swallowed by the fresh-login
    // fail-open: an unsupported `--shell` value keeps its promised exit 2
    // even when `shell.integration` is invalid at the same time.
    let (code, stdout, stderr) = run_failopen_cli(
        &["--shell", "fish", "--login"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_INTEGRATION", "bogus"),
            ("COSH_SHELL_DEFAULT_SHELL", "/bin/bash"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        b"echo FALLOPEN_SHELL_ARG_MARKER\nexit\n",
    );
    let visible = strip_ansi_escape(&stdout).replace('\r', "");
    assert_eq!(code, 2, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stderr.contains("unsupported raw shell: fish"),
        "the shell argument error must be reported: {stderr}"
    );
    assert!(
        !stderr.contains("falling back to a native login shell"),
        "an explicit CLI error must not enter the fail-open fallback: {stderr}"
    );
    assert!(
        !visible.contains("FALLOPEN_SHELL_ARG_MARKER"),
        "no shell may be started for a rejected --shell value: {stdout}"
    );
    let _ = fs::remove_dir_all(_home);
}
