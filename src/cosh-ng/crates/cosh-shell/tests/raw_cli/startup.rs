use super::*;
#[cfg(target_os = "linux")]
use crate::support::terminal_screen::TerminalSession;
use ratatui::text::Span;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[test]
fn help_flag_writes_to_stdout_not_stderr() {
    // POSIX/GNU convention: --help output goes to stdout, not stderr.
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = Command::new(binary)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn cosh-shell --help");

    assert!(output.status.success(), "--help should exit 0");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("Usage: cosh-shell"),
        "--help text should be on stdout, got stdout={:?}",
        stdout
    );
    assert!(
        stderr.is_empty(),
        "--help should not write to stderr, got stderr={:?}",
        stderr
    );
}

#[test]
fn version_flag_writes_to_stdout_not_stderr() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn cosh-shell --version");

    assert!(output.status.success(), "--version should exit 0");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("cosh-shell"),
        "--version text should be on stdout, got stdout={:?}",
        stdout
    );
    assert!(
        stderr.is_empty(),
        "--version should not write to stderr, got stderr={:?}",
        stderr
    );
}

#[test]
fn diagnostics_export_help_writes_to_stdout_not_stderr() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = Command::new(binary)
        .args(["diagnostics", "export", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn cosh-shell diagnostics export --help");

    assert!(
        output.status.success(),
        "diagnostics export --help should exit 0"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("Usage:"),
        "diagnostics export --help should write to stdout, got stdout={:?}",
        stdout
    );
    assert!(
        stderr.is_empty(),
        "diagnostics export --help should not write to stderr, got stderr={:?}",
        stderr
    );
}

#[test]
fn raw_cli_startup_banner_renders_when_enabled() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("Adapter: fake"), "{output}");
    assert!(output.contains("Shell: bash"), "{output}");
    assert!(output.contains("Approval: auto"), "{output}");
    assert!(output.contains("Analysis: smart"), "{output}");
    assert!(!output.contains("Mode: auto"), "{output}");
    assert!(output.contains("/help"), "{output}");
    assert!(output.contains("/hooks"), "{output}");
    assert!(!output.contains("/explain"), "{output}");
    assert!(
        !output.contains("┌─┐┌─┐┌─┐┬ ┬"),
        "logo should be removed: {output}"
    );
    assert!(
        !output.contains("Agent actions still require approval"),
        "footer should be removed: {output}"
    );
    assert!(
        !output.contains("Startup hooks: none configured"),
        "no hooks line when hooks are disabled: {output}"
    );
    assert!(!output.contains("no command ran"), "{output}");
    assert!(!output.contains("cosh-osc$ ╭ · cosh-shell"), "{output}");
    assert_inline_before_followup(&output, "╭ · cosh-shell", "exit");
    assert!(!output.contains("Thinking..."), "{output}");
}

#[test]
fn raw_cli_startup_banner_renders_cached_upgrade_notice() {
    let directory = temp_shell_home("startup-upgrade-cache");
    let cache = directory.join("upgrade-check.json");
    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_cosh-shell"))
        .expect("canonical cosh-shell executable");
    let fixture = serde_json::json!({
        "version": 1,
        "checked_at": 0,
        "executable": executable,
        "method": "anolisa",
        "notice": {
            "package": "cosh-ng",
            "current": "0.1.0",
            "latest": "99.0.0",
            "command": "anolisa update cosh-ng"
        }
    });
    fs::write(
        &cache,
        serde_json::to_vec(&fixture).expect("serialize upgrade cache"),
    )
    .expect("write upgrade cache");
    let cache = cache.to_string_lossy().into_owned();
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_UPGRADE_CHECK_CACHE", &cache),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
    );

    assert!(output.contains("Update available: cosh-ng"), "{output}");
    assert!(
        output.contains(&format!("cosh-ng {} → 99.0.0", env!("CARGO_PKG_VERSION"))),
        "{output}"
    );
    assert!(!output.contains("cosh-ng 0.1.0 → 99.0.0"), "{output}");
    assert!(output.contains("anolisa update cosh-ng"), "{output}");
}

#[cfg(target_os = "linux")]
fn assert_upgrade_uses_login_path(profile_only: bool, profile_times_out: bool, ai_enabled: bool) {
    let home = temp_shell_home("upgrade-login-path");
    let bin = home.join("bin");
    fs::create_dir(&bin).expect("create upgrade bin");
    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_cosh-shell")).unwrap();
    let status = serde_json::json!({"ok": true, "data": {"components": [{
        "active": true, "scope": "system", "health": [{
            "name": format!("integrity:{}", executable.display())
        }]
    }]}});
    write_executable(&bin.join("anolisa"), &format!(
        "#!/bin/sh\nprintf '%s\\n' \"$PATH\" >> \"$HOME/upgrade-path\"\ncase \"$*\" in\n'status cosh-ng --json') printf '%s\\n' '{status}';;\n'--install-mode system update cosh-ng --dry-run --json') printf '%s\\n' '{{\"ok\":true,\"data\":{{\"to_version\":\"99.0.0\",\"plan\":[\"update\"]}}}}';;\n*) exit 1;;\nesac\n"
    ));
    let delay = if profile_times_out { "6" } else { "0.2" };
    fs::write(home.join(".bash_profile"),
        format!("sleep {delay}\nprintf 'profile-hit\\n' >> \"$HOME/profile-hits\"\nexport PATH=\"$HOME/bin:$PATH\"\nPS1='upgrade-login$ '\n")
    ).unwrap();
    let inherited = if profile_only {
        "/usr/bin:/bin".to_owned()
    } else {
        format!("{}:/usr/bin:/bin", bin.display())
    };
    let ready = if profile_times_out || !ai_enabled {
        "upgrade-login$ "
    } else {
        "Update available: cosh-ng"
    };
    let input: &[u8] = if ai_enabled {
        b"exit\n"
    } else {
        b"sleep 1; exit\n"
    };
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash", "--login"],
        &[
            ("HOME", &home.display().to_string()),
            ("PATH", &inherited),
            ("COSH_SHELL_AI", if ai_enabled { "on" } else { "off" }),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LOGIN_IDENTITY", "1"),
            ("COSH_SHELL_BOOTSTRAP_PATH", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_UPGRADE_CHECK", "1"),
            (
                "COSH_SHELL_UPGRADE_CHECK_CACHE",
                &home.join("upgrade-cache.json").display().to_string(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        &home,
        &[(ready, input)],
    );
    assert_eq!(
        fs::read_to_string(home.join("profile-hits"))
            .unwrap()
            .lines()
            .count(),
        1,
        "{output}"
    );
    if profile_times_out || !ai_enabled {
        assert!(
            !home.join("upgrade-path").exists(),
            "AI-off or unresolved startup PATH must not launch an upgrade probe: {output}"
        );
        assert!(!home.join("upgrade-cache.json").exists(), "{output}");
        fs::remove_dir_all(home).unwrap();
        return;
    }
    assert!(output.contains("Update available: cosh-ng"), "{output}");
    let paths = fs::read_to_string(home.join("upgrade-path")).expect("upgrade command PATH");
    assert_eq!(paths.lines().count(), 2, "{paths}");
    assert!(
        paths
            .lines()
            .all(|path| path.starts_with(&format!("{}:", bin.display()))),
        "{paths}"
    );
    fs::remove_dir_all(home).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_probe_uses_profile_only_anolisa_once() {
    assert_upgrade_uses_login_path(true, false, true);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_probe_keeps_inherited_path_control() {
    assert_upgrade_uses_login_path(false, false, true);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_probe_skips_an_unresolved_startup_path() {
    assert_upgrade_uses_login_path(false, true, true);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_probe_ai_off_blocks_profile_only_anolisa() {
    assert_upgrade_uses_login_path(true, false, false);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_probe_uses_profile_only_rpm_and_package_manager() {
    let home = temp_shell_home("upgrade-login-rpm-path");
    let bin = home.join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(&bin.join("anolisa"), "#!/bin/sh\nexit 1\n");
    write_executable(&bin.join("rpm"),
        "#!/bin/sh\nprintf 'rpm:%s\\n' \"$PATH\" >> \"$HOME/upgrade-path\"\nprintf 'cosh-ng\\n0:0.1.0-1\\n'\n");
    write_executable(&bin.join("dnf"),
        "#!/bin/sh\nprintf 'dnf:%s\\n' \"$PATH\" >> \"$HOME/upgrade-path\"\nprintf 'cosh-ng.aarch64 99.0.0-1 fixture\\n'\nexit 100\n");
    fs::write(home.join(".bash_profile"),
        "printf 'profile-hit\\n' >> \"$HOME/profile-hits\"\nexport PATH=\"$HOME/bin:$PATH\"\nPS1='upgrade-rpm$ '\n").unwrap();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash", "--login"],
        &[
            ("HOME", &home.display().to_string()),
            ("PATH", "/usr/bin:/bin"),
            ("COSH_SHELL_AI", "on"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LOGIN_IDENTITY", "1"),
            ("COSH_SHELL_BOOTSTRAP_PATH", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_UPGRADE_CHECK", "1"),
            (
                "COSH_SHELL_UPGRADE_CHECK_CACHE",
                &home.join("upgrade-cache.json").display().to_string(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        &home,
        &[("Update available: cosh-ng", b"exit\n")],
    );
    assert!(output.contains("sudo dnf update cosh-ng"), "{output}");
    assert_eq!(
        fs::read_to_string(home.join("profile-hits"))
            .unwrap()
            .lines()
            .count(),
        1,
        "{output}"
    );
    let paths = fs::read_to_string(home.join("upgrade-path")).unwrap();
    let expected_path = format!("{}:", bin.display());
    assert_eq!(paths.lines().count(), 2, "{paths}");
    assert!(
        paths
            .lines()
            .any(|line| line.starts_with(&format!("rpm:{expected_path}"))),
        "{paths}"
    );
    assert!(
        paths
            .lines()
            .any(|line| line.starts_with(&format!("dnf:{expected_path}"))),
        "{paths}"
    );
    fs::remove_dir_all(home).unwrap();
}

#[cfg(target_os = "linux")]
const UPGRADE_PROBE_RELEASE_MARKER: &str = "upgrade-probe-release";
#[cfg(target_os = "linux")]
const UPGRADE_PROBE_PID_FILE: &str = "upgrade-probe.pid";

#[cfg(target_os = "linux")]
fn spawn_startup_upgrade_policy_fixture(
    ai: Option<&str>,
    banner_enabled: bool,
    upgrade_enabled: bool,
    cached: bool,
) -> TerminalSession {
    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_cosh-shell"))
        .expect("canonical cosh-shell executable");
    let current_evr = format!("{}-1", env!("CARGO_PKG_VERSION"));
    let cache = serde_json::json!({
        "version": 1,
        "checked_at": 0,
        "executable": executable,
        "method": "rpm",
        "notice": {
            "package": "cosh-ng",
            "current": current_evr,
            "latest": "99.0.0-1",
            "command": "sudo dnf update cosh-ng"
        }
    })
    .to_string();
    let rpm = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/rpm-probe.calls\"\n\
         printf 'cosh-ng\\n0:{}-1\\n'\n",
        env!("CARGO_PKG_VERSION")
    );
    let mut files = vec![
        (
            "bin/anolisa",
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/anolisa-probe.calls\"\nexit 1\n",
        ),
        ("bin/rpm", rpm.as_str()),
        (
            "bin/dnf",
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/dnf-probe.calls\"\n\
             printf 'cosh-ng.x86_64 99.0.0-1 fixture\\n'\nexit 100\n",
        ),
    ];
    if cached {
        files.push(("upgrade-cache.json", cache.as_str()));
    }
    let mut envs = vec![
        ("PATH", "$HOME/bin:/usr/bin:/bin"),
        (
            "COSH_SHELL_STARTUP_BANNER",
            if banner_enabled { "1" } else { "0" },
        ),
        (
            "COSH_SHELL_UPGRADE_CHECK",
            if upgrade_enabled { "on" } else { "off" },
        ),
        ("COSH_SHELL_UPGRADE_CHECK_CACHE", "$HOME/upgrade-cache.json"),
    ];
    if let Some(ai) = ai {
        envs.push(("COSH_SHELL_AI", ai));
    }
    TerminalSession::spawn_for_shell_with_startup_env("bash", "enhanced", 200, &files, &envs)
}

#[cfg(target_os = "linux")]
fn assert_no_startup_upgrade_probe(mut session: TerminalSession) {
    session.send(b"sleep 1; printf '\\nupgrade-%s\\n' observed\n");
    session.wait_screen("startup observation window completes", |screen| {
        screen.contents().contains("\nupgrade-observed")
    });
    assert!(
        !session.home().join("anolisa-probe.calls").exists(),
        "startup launched the background ownership probe"
    );
    assert!(
        !session.home().join("dnf-probe.calls").exists(),
        "startup launched a package upgrade check"
    );
    session.finish();
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_startup_upgrade_policy_ai_off_blocks_probe() {
    assert_no_startup_upgrade_probe(spawn_startup_upgrade_policy_fixture(
        Some("off"),
        true,
        true,
        false,
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_startup_upgrade_policy_ai_off_preserves_cache_and_rpm_lookup() {
    let session = spawn_startup_upgrade_policy_fixture(Some("off"), true, true, true);
    let contents = session.screen().contents();
    assert!(contents.contains("Update available: cosh-ng"), "{contents}");
    assert!(contents.contains("sudo dnf update cosh-ng"), "{contents}");
    assert!(
        session.home().join("rpm-probe.calls").exists(),
        "cached RPM notice must retain its local ownership lookup"
    );
    assert_no_startup_upgrade_probe(session);
}

#[cfg(target_os = "linux")]
fn assert_startup_upgrade_probe_runs(ai: Option<&str>) {
    let mut session = spawn_startup_upgrade_policy_fixture(ai, true, true, false);
    session.wait_screen("AI-enabled startup completes its package probe", |screen| {
        screen.contents().contains("Update available: cosh-ng")
    });
    let calls = fs::read_to_string(session.home().join("dnf-probe.calls"))
        .expect("AI-enabled startup executes fixture dnf");
    assert_eq!(calls.trim(), "check-update cosh-ng");
    session.finish();
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_startup_upgrade_policy_ai_on_executes_probe() {
    assert_startup_upgrade_probe_runs(Some("on"));
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_startup_upgrade_policy_default_ai_executes_probe() {
    assert_startup_upgrade_probe_runs(None);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_startup_upgrade_policy_upgrade_optout_blocks_probe() {
    assert_no_startup_upgrade_probe(spawn_startup_upgrade_policy_fixture(
        Some("on"),
        true,
        false,
        false,
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_startup_upgrade_policy_banner_optout_blocks_probe() {
    assert_no_startup_upgrade_probe(spawn_startup_upgrade_policy_fixture(
        Some("on"),
        false,
        true,
        false,
    ));
}

/// Spawns bash with a fake system-scope anolisa whose status probe records its PID in
/// `UPGRADE_PROBE_PID_FILE` and blocks until the test writes `UPGRADE_PROBE_RELEASE_MARKER`,
/// so the first login has no cache and the probe result lands after the banner.
#[cfg(target_os = "linux")]
fn spawn_with_gated_upgrade_probe() -> TerminalSession {
    spawn_with_gated_upgrade_probe_inputrc(None)
}

#[cfg(target_os = "linux")]
fn spawn_with_gated_upgrade_probe_inputrc(inputrc: Option<&str>) -> TerminalSession {
    let files: Vec<_> = inputrc
        .map(|inputrc| ("inputrc", inputrc))
        .into_iter()
        .collect();
    spawn_with_gated_upgrade_probe_for_shell("bash", &files)
}

#[cfg(target_os = "linux")]
fn spawn_with_gated_upgrade_probe_for_shell(
    shell: &str,
    extra_files: &[(&str, &str)],
) -> TerminalSession {
    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_cosh-shell"))
        .expect("canonical cosh-shell executable");
    let marker = UPGRADE_PROBE_RELEASE_MARKER;
    let pid_file = UPGRADE_PROBE_PID_FILE;
    let status = serde_json::json!({
        "ok": true,
        "data": {
            "components": [{
                "active": true,
                "scope": "system",
                "health": [{ "name": format!("integrity:{}", executable.display()) }]
            }]
        }
    });
    let update = serde_json::json!({
        "ok": true,
        "data": {
            "package": "cosh-ng",
            "to_version": "99.0.0",
            "plan": ["update cosh-ng"]
        }
    });
    // A system record probed from a non-root session only succeeds with an explicit scope;
    // the default user-mode update is rejected like anolisa rejects read-only system records.
    let anolisa = format!(
        "#!/bin/sh\ncase \"$*\" in\n  'status cosh-ng --json')\n    printf '%s\\n' \"$$\" > \"$HOME/{pid_file}\"\n    while [ ! -f \"$HOME/{marker}\" ]; do sleep 0.01; done\n    printf '%s\\n' '{status}'\n    ;;\n  '--install-mode system update cosh-ng --dry-run --json')\n    printf '%s\\n' '{update}'\n    ;;\n  *)\n    exit 1\n    ;;\nesac\n"
    );
    let path = "$HOME/bin:/usr/bin:/bin";
    let mut files = vec![("bin/anolisa", anolisa.as_str())];
    files.extend_from_slice(extra_files);
    // Wide enough that the notice panel keeps the scoped command on one row.
    TerminalSession::spawn_for_shell_with_startup_env(
        shell,
        "enhanced",
        200,
        &files,
        &[
            ("PATH", path),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_UPGRADE_CHECK_CACHE", "$HOME/upgrade-cache.json"),
        ],
    )
}

#[cfg(target_os = "linux")]
fn release_upgrade_probe(session: &TerminalSession) {
    fs::write(
        session.home().join(UPGRADE_PROBE_RELEASE_MARKER),
        "release probe",
    )
    .expect("release upgrade probe");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !session.home().join("upgrade-cache.json").exists() {
        assert!(
            Instant::now() < deadline,
            "upgrade probe result was not delivered"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_notice_renders_at_an_idle_prompt_without_input() {
    let mut session = spawn_with_gated_upgrade_probe();
    session.wait_screen("startup banner before upgrade probe completes", |screen| {
        let contents = screen.contents();
        contents.contains("╭ · cosh-shell") && !contents.contains("Update available: cosh-ng")
    });
    release_upgrade_probe(&session);

    session.wait_screen("idle prompt renders deferred upgrade notice", |screen| {
        let contents = screen.contents();
        contents.contains("Update available: cosh-ng")
            && contents.contains("sudo anolisa --install-mode system update cosh-ng")
    });
    session.finish();
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_notice_skips_the_session_after_typed_input() {
    let mut session = spawn_with_gated_upgrade_probe();

    let draft = "echo draft-preserved";
    session.send(draft.as_bytes());
    session.wait_screen("draft before upgrade probe completes", |screen| {
        screen.contents().contains(draft)
    });
    release_upgrade_probe(&session);

    session.send(b"\x01");
    session.wait_screen("completed probe preserves active draft", |screen| {
        let contents = screen.contents();
        contents.contains(draft) && !contents.contains("Update available: cosh-ng")
    });

    // Typed keys are zero-count writes that may hide a custom accept-line
    // binding, so submitting the draft leaves the prompt occupied for good.
    session.send(b"\n");
    session.wait_screen("typed draft runs", |screen| {
        screen.contents().contains("\ndraft-preserved")
    });
    assert_upgrade_notice_skipped_for_the_session(&mut session);
    session.finish_with_input(b"exit 0\n", 0);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_notice_waits_until_the_next_prompt_after_a_failed_command() {
    let mut session = spawn_with_gated_upgrade_probe();

    // One write carries the whole line, so its submit is counted and the
    // prompt after the failure is a `CommandFailed` boundary.
    session.send(b"sleep 1; printf 'failed-%s\\n' command; false\n");
    release_upgrade_probe(&session);

    session.wait_screen("next prompt renders deferred upgrade notice", |screen| {
        let contents = screen.contents();
        contents.contains("Update available: cosh-ng")
            && contents.contains("sudo anolisa --install-mode system update cosh-ng")
    });
    let contents = session.screen().contents();
    let command_output = contents
        .rfind("failed-command")
        .expect("failed command output");
    let notice = contents
        .rfind("Update available: cosh-ng")
        .expect("deferred upgrade notice");
    assert!(
        command_output < notice,
        "upgrade notice rendered before the failed command completed:\n{contents}"
    );
    session.finish_with_input(b"exit 0\n", 0);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_notice_preserves_input_after_a_completed_command() {
    let mut session = spawn_with_gated_upgrade_probe();

    session.send(b"printf 'first-command-done\\n'\n");
    session.wait_screen("first command reaches its next prompt", |screen| {
        let contents = screen.contents();
        contents.contains("first-command-done") && contents.trim_end().ends_with("screen$")
    });

    let draft = "echo second-command-draft";
    session.send(draft.as_bytes());
    session.wait_screen("second command draft", |screen| {
        screen.contents().contains(draft)
    });
    let prompt_column = session.prompt().len() as u16;
    session.send(b"\x01");
    session.wait_screen("second draft reports input empty", |screen| {
        screen.contents().contains(draft) && screen.cursor_position().1 == prompt_column
    });

    release_upgrade_probe(&session);
    thread::sleep(Duration::from_millis(1500));

    // Force the fixture to drain output produced by the idle poll. Before the
    // prompt barrier fix, that poll inserted the notice into this active draft.
    session.send(b"\x05");
    session.wait_screen("completed probe preserves the second draft", |screen| {
        let contents = screen.contents();
        contents.contains(draft) && !contents.contains("Update available: cosh-ng")
    });

    // The typed draft is zero-count input, so its submission keeps the prompt
    // occupied for the rest of the login.
    session.send(b"\n");
    session.wait_screen("second command runs", |screen| {
        screen.contents().contains("\nsecond-command-draft")
    });
    assert_upgrade_notice_skipped_for_the_session(&mut session);
    session.finish_with_input(b"exit 0\n", 0);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_notice_skips_the_session_after_custom_accept_line_typeahead() {
    // `C-x` is also a Readline key-sequence prefix, so the binding fires only
    // after `keyseq-timeout` while the PTY stays silent; no quiet window can
    // tell that C is still queued.
    let inputrc = "set editing-mode emacs\nset enable-bracketed-paste on\n\"\\C-x\": accept-line\n";
    let mut session = spawn_with_gated_upgrade_probe_inputrc(Some(inputrc));

    session.send(b"printf 'A-%s\\n' started; sleep 2; printf 'A-%s\\n' done\n");
    session.wait_screen("first command is still running", |screen| {
        screen.contents().contains("A-started")
    });

    session.send(b"sleep 1; printf 'B-%s\\n' done\x18");
    session.send(b"sleep 1; printf 'C-%s\\n' done\x18");
    release_upgrade_probe(&session);

    session.wait_screen("consecutive custom accept-line typeahead runs", |screen| {
        screen.contents().contains("C-done")
    });
    assert_upgrade_notice_skipped_for_the_session(&mut session);
    session.finish_with_input(b"exit 0\n", 0);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_notice_skips_the_zsh_session_after_typed_input() {
    // Ordinary zsh keystrokes typed before Enter are zero-count writes, which
    // byte counting cannot tell apart from a custom accept-line widget.
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    let mut session = spawn_with_gated_upgrade_probe_for_shell("zsh", &[]);
    session.wait_screen("startup banner before upgrade probe completes", |screen| {
        screen.contents().contains("╭ · cosh-shell")
    });

    // Wait for the echo so the leading keystrokes cannot coalesce with the
    // submit into one counted write; the relay must see a zero-count write.
    session.send(b"echo ");
    session.wait_screen("leading zsh keystrokes echo", |screen| {
        screen.contents().contains("screen$ echo")
    });
    session.send(b"first-zsh-done\n");
    session.wait_screen("first zsh command completes", |screen| {
        screen.contents().contains("\nfirst-zsh-done")
    });

    release_upgrade_probe(&session);
    assert_upgrade_notice_skipped_for_the_session(&mut session);
    session.finish_with_input(b"exit 0\n", 0);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_notice_skips_the_session_after_zsh_bindkey_typeahead() {
    // ZLE `bindkey` submissions are as invisible to byte counting as Readline
    // `bind` ones. `^T` is no ZLE prefix, so every queued command runs.
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    let zshrc = "PROMPT='screen$ '\nRPROMPT=''\nbindkey -e\nbindkey '^T' accept-line\n";
    let mut session = spawn_with_gated_upgrade_probe_for_shell("zsh", &[(".zshrc", zshrc)]);

    session.send(b"printf 'A-%s\\n' started; sleep 2; printf 'A-%s\\n' done\n");
    session.wait_screen("first zsh command is still running", |screen| {
        screen.contents().contains("A-started")
    });

    session.send(b"sleep 1; printf 'B-%s\\n' done\x14");
    session.send(b"sleep 1; printf 'C-%s\\n' done\x14");
    release_upgrade_probe(&session);

    session.wait_screen("consecutive zsh bindkey typeahead runs", |screen| {
        screen.contents().contains("C-done")
    });
    assert_upgrade_notice_skipped_for_the_session(&mut session);
    session.finish_with_input(b"exit 0\n", 0);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_upgrade_notice_waits_for_a_queued_typeahead_command() {
    let mut session = spawn_with_gated_upgrade_probe();

    session.send(b"printf 'A-%s\\n' started; sleep 2; printf 'A-%s\\n' done\n");
    session.wait_screen("first command is still running", |screen| {
        screen.contents().contains("A-started")
    });

    session.send(b"sleep 1; printf 'B-%s\\n' done\n");
    release_upgrade_probe(&session);

    session.wait_screen("notice follows the queued typeahead command", |screen| {
        let contents = screen.contents();
        contents.contains("B-done") && contents.contains("Update available: cosh-ng")
    });
    let contents = session.screen().contents();
    let command_output = contents
        .rfind("B-done")
        .expect("queued typeahead command output");
    let notice = contents
        .rfind("Update available: cosh-ng")
        .expect("deferred upgrade notice");
    assert!(
        command_output < notice,
        "upgrade notice rendered before queued typeahead completed:\n{contents}"
    );

    session.finish_with_input(b"exit 0\n", 0);
}

/// Asserts that held prompt occupancy skips the deferred notice for the rest
/// of the login: neither an idle prompt that outlasts any default line-editor
/// key timeout nor the prompt boundaries of later counted submissions may
/// render it.
#[cfg(target_os = "linux")]
fn assert_upgrade_notice_skipped_for_the_session(session: &mut TerminalSession) {
    thread::sleep(Duration::from_millis(1500));
    session.send(b"printf 'after-%s\\n' idle\n");
    session.wait_screen("counted submission after the idle prompt", |screen| {
        screen.contents().contains("\nafter-idle")
    });
    // A second command drains anything the first boundary rendered late.
    session.send(b"printf 'final-%s\\n' check\n");
    session.wait_screen("second counted submission", |screen| {
        screen.contents().contains("\nfinal-check")
    });
    // Raw output also covers a notice that already scrolled off the screen.
    let output = session.raw_output();
    assert!(
        !output.contains("Update available: cosh-ng"),
        "deferred upgrade notice rendered after zero-count input:\n{output}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_exit_kills_an_in_flight_upgrade_probe() {
    let session = spawn_with_gated_upgrade_probe();
    let pid_file = session.home().join(UPGRADE_PROBE_PID_FILE);
    let deadline = Instant::now() + Duration::from_secs(5);
    let probe = loop {
        let pid = fs::read_to_string(&pid_file).ok();
        if let Some(pid) = pid.and_then(|pid| pid.trim().parse::<u32>().ok()) {
            break pid;
        }
        assert!(Instant::now() < deadline, "upgrade probe did not start");
        thread::sleep(Duration::from_millis(10));
    };

    // The probe is still blocked on the release marker, so only shell shutdown can stop it.
    session.finish();
    let deadline = Instant::now() + Duration::from_secs(5);
    let process = std::path::PathBuf::from(format!("/proc/{probe}"));
    while process.exists() {
        assert!(
            Instant::now() < deadline,
            "upgrade probe {probe} was not reaped after shell exit"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_rpm_cache_check_does_not_hold_the_first_prompt_after_rpm_exits() {
    assert_rpm_cache_check_releases_first_prompt("true");
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_rpm_cache_check_does_not_hold_the_first_prompt_after_rpm_times_out() {
    assert_rpm_cache_check_releases_first_prompt("exec sleep 30");
}

/// Seeds an RPM-method cache so startup revalidates it with a fake `rpm` whose first call
/// leaves a descendant in its own session holding the pipes, then runs `first_call_tail`.
#[cfg(target_os = "linux")]
fn assert_rpm_cache_check_releases_first_prompt(first_call_tail: &str) {
    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_cosh-shell"))
        .expect("canonical cosh-shell executable");
    let cache = serde_json::json!({
        "version": 1,
        "checked_at": 0,
        "executable": executable,
        "method": "rpm",
        "notice": {
            "package": "cosh-ng",
            "current": "1.0.0-1",
            "latest": "99.0.0-1",
            "command": "sudo dnf update cosh-ng"
        }
    })
    .to_string();
    let rpm = format!(
        "#!/bin/sh\nif mkdir \"$HOME/rpm-first-probe\" 2>/dev/null; then\n  \
         setsid sh -c 'echo $$ > \"$0\"; exec sleep 30' \"$HOME/escaped.pid\" &\n  \
         while [ ! -s \"$HOME/escaped.pid\" ]; do sleep 0.01; done\n  {first_call_tail}\nfi\n\
         printf 'cosh-ng\\n1.0.0-1\\n'\n"
    );
    let started = Instant::now();
    let session = TerminalSession::spawn_for_shell_with_startup_env(
        "bash",
        "enhanced",
        100,
        &[("bin/rpm", &rpm), ("upgrade-cache.json", &cache)],
        &[
            ("PATH", "$HOME/bin:/usr/bin:/bin"),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_UPGRADE_CHECK_CACHE", "$HOME/upgrade-cache.json"),
        ],
    );
    let elapsed = started.elapsed();
    let pid = fs::read_to_string(session.home().join("escaped.pid")).unwrap_or_default();
    if let Ok(pid) = pid.trim().parse::<i32>() {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    session.finish();

    assert!(
        !pid.is_empty(),
        "fake rpm was not consulted for the RPM cache"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "first prompt waited {elapsed:?}"
    );
}

#[test]
fn raw_cli_startup_banner_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_LANG", "zh-CN"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("后端: fake"), "{output}");
    assert!(output.contains("Shell: bash"), "{output}");
    assert!(output.contains("审批: auto"), "{output}");
    assert!(output.contains("分析: smart"), "{output}");
    assert!(!output.contains("模式: auto"), "{output}");
    assert!(output.contains("/help"), "{output}");
}

#[test]
fn raw_cli_startup_banner_reports_effective_modes() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_APPROVAL_MODE", "trust"),
            ("COSH_SHELL_ANALYSIS_MODE", "manual"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("Approval: trust"), "{output}");
    assert!(output.contains("Analysis: manual"), "{output}");
}

#[test]
fn raw_cli_startup_approval_alias_and_invalid_value_fail_closed() {
    for value in ["balanced", "invalid"] {
        let output = run_raw_cli_with_env(
            "fake",
            "exit\n",
            &[
                ("COSH_SHELL_STARTUP_BANNER", "1"),
                ("COSH_SHELL_APPROVAL_MODE", value),
                ("TERM", "xterm-256color"),
            ],
        );

        assert!(output.contains("Approval: recommend"), "{value}: {output}");
        assert!(!output.contains("Approval: auto"), "{value}: {output}");
    }
}

#[test]
fn raw_cli_plain_startup_banner_keeps_both_modes() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_RENDER", "plain"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("Approval: auto"), "{output}");
    assert!(output.contains("Analysis: smart"), "{output}");
}

#[test]
fn raw_cli_startup_health_fixture_renders_when_enabled() {
    let cwd = temp_shell_home("startup-health-fixture");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("warning"), "{output}");
    assert!(output.contains("Load  1m"), "{output}");
    assert!(!output.contains("Load  Load 1m"), "{output}");
    assert!(output.contains("Mem used"), "{output}");
    assert!(!output.contains("Mem avail 94%"), "{output}");
    assert!(output.contains("Swap used"), "{output}");
    assert!(output.contains("Findings"), "{output}");
    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(output.contains("[Health]"), "{output}");
    assert_eq!(output.matches("[Health]").count(), 3, "{output}");
    assert!(!output.contains("Next:"), "{output}");
    assert_inline_before_followup(&output, "╭─ Health check", "exit");
}

#[test]
fn raw_cli_startup_health_disabled_renders_no_row() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "disabled"),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("/help"), "{output}");
    assert!(!output.contains("Health: ok"), "{output}");
    assert!(!output.contains("Health check"), "{output}");
}

#[test]
fn raw_cli_startup_health_cards_share_configured_width() {
    let cwd = temp_shell_home("startup-health-card-width");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_WIDTH", "140"),
            ("TERM", "xterm-256color"),
        ],
        &cwd,
        vec![
            (b"?? hello\n".to_vec(), Duration::from_millis(1600)),
            (b"exit\n".to_vec(), Duration::from_millis(900)),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("Received shell prompt request"), "{output}");
    let widths = box_line_widths(&output);
    assert!(
        widths.len() >= 3,
        "expected startup, health and agent boxes\n{output}"
    );
    assert!(
        widths.iter().all(|width| *width == 140),
        "box widths should match configured width: {widths:?}\n{output}"
    );
}

#[test]
fn raw_cli_startup_health_critical_fixture_uses_compact_oom_copy() {
    let cwd = temp_shell_home("startup-health-critical-fixture");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-critical"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("critical"), "{output}");
    assert!(output.contains("OOM"), "{output}");
    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(
        output.contains("cause of the most recent OOM")
            || output.contains("why the latest OOM killed"),
        "{output}"
    );
    assert!(!output.contains("CONSTRAINT_"), "{output}");
    assert!(!output.contains("current pressure"), "{output}");
    assert!(!output.contains("OOM age"), "{output}");
    assert!(!output.contains("process python"), "{output}");
    assert!(!output.contains("pid "), "{output}");
    assert!(!output.contains("constraint "), "{output}");
    assert!(!output.contains("Next:"), "{output}");
    assert!(!output.contains("██████████"), "{output}");
}

#[test]
fn raw_cli_startup_health_degraded_fixture_is_read_only() {
    let cwd = temp_shell_home("startup-health-degraded-read-only");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-degraded"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        vec![
            (b"\t\n".to_vec(), Duration::from_millis(1400)),
            (b"exit\n".to_vec(), Duration::from_millis(700)),
        ],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("degraded"), "{output}");
    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(output.contains("[Health]"), "{output}");
    assert!(!output.contains("[Personal]"), "{output}");
    assert!(!output.contains("Tab insert"), "{output}");
    assert!(!output.contains("Enter ask"), "{output}");
    assert!(!output.contains("Shift+Tab cycle"), "{output}");
    assert!(
        !output.contains("Received shell prompt request:"),
        "{output}"
    );
}

#[test]
fn raw_cli_startup_health_healthy_fixture_renders_startup_row_in_banner() {
    let cwd = temp_shell_home("startup-health-healthy-fixture");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-healthy"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
        &cwd,
        vec![(b"exit\n".to_vec(), Duration::from_millis(150))],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("Health: ok"), "{output}");
    // The banner path marks the report rendered, so the deferred path must
    // not emit the row a second time.
    assert_eq!(output.matches("Health: ok").count(), 1, "{output}");
    assert!(output.contains("Mem used"), "{output}");
    assert!(output.contains("Disk"), "{output}");
    assert!(!output.contains("Health check"), "{output}");
    assert!(!output.contains("Suggested prompts"), "{output}");
    assert_inline_before_followup(&output, "╭ · cosh-shell", "exit");
}

#[test]
fn raw_cli_startup_health_no_color_keeps_readable_content() {
    let cwd = temp_shell_home("startup-health-no-color");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("NO_COLOR", "1"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("warning"), "{output}");
    assert!(output.contains("Load  1m"), "{output}");
    assert!(!output.contains("Load  Load 1m"), "{output}");
    assert!(output.contains("Mem used"), "{output}");
    assert!(!output.contains("Mem avail 94%"), "{output}");
    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(output.contains("[Health]"), "{output}");
    assert!(!output.contains("Next:"), "{output}");
    let health_block = output
        .split("Health check")
        .nth(1)
        .unwrap_or(output.as_str())
        .split("Suggested prompts")
        .next()
        .unwrap_or(output.as_str());
    assert!(!health_block.contains("\x1b["), "{output}");
}

#[test]
fn raw_cli_startup_health_dumb_terminal_uses_plain_fallback() {
    let cwd = temp_shell_home("startup-health-dumb");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "dumb"),
        ],
        &cwd,
        &[("Mem used", b"exit\n")],
    );

    assert!(output.contains("Health check:"), "{output}");
    assert!(output.contains("warning"), "{output}");
    assert!(output.contains("Load  1m"), "{output}");
    assert!(!output.contains("Load  Load 1m"), "{output}");
    assert!(output.contains("Mem used"), "{output}");
    assert!(!output.contains("Mem avail 94%"), "{output}");
    assert!(!output.contains("Suggested prompts"), "{output}");
    assert!(!output.contains("Shift+Tab cycle"), "{output}");
    assert!(!output.contains("▕"), "{output}");
    assert!(!output.contains("Next:"), "{output}");
    assert!(!output.contains('╭'), "{output}");
    assert!(!output.contains('│'), "{output}");
    assert!(!output.contains('╰'), "{output}");
}

#[test]
fn raw_cli_startup_health_suppresses_all_three_visible_try_items() {
    let cwd = temp_shell_home("startup-health-suppressed-try");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let env = [
        ("COSH_SHELL_STARTUP_BANNER", "1"),
        ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
        (
            "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
            suppression_store.as_str(),
        ),
        ("COSH_SHELL_LANG", "en-US"),
        ("TERM", "xterm-256color"),
        ("COSH_SHELL_ISOLATED", "0"),
    ];

    let first = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &env,
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );
    assert!(first.contains("Suggested prompts"), "{first}");
    assert!(!first.contains("Next:"), "{first}");

    let second = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &env,
        &cwd,
        vec![(b"exit\n".to_vec(), Duration::from_millis(150))],
    );
    assert!(second.contains("Health check"), "{second}");
    assert!(second.contains("warning"), "{second}");
    assert!(!second.contains("Suggested prompts"), "{second}");
    assert!(first_health_prompt(&second).is_none(), "{second}");
    assert!(!second.contains("Next:"), "{second}");
}

#[test]
fn raw_cli_startup_health_banner_disabled_does_not_suppress_later_prompt() {
    let cwd = temp_shell_home("startup-health-hidden-no-suppress");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store_str = suppression_store.to_string_lossy().into_owned();

    let hidden = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store_str.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        vec![(b"exit\n".to_vec(), Duration::from_millis(150))],
    );
    assert!(!hidden.contains("Health check"), "{hidden}");
    assert!(
        !suppression_store.exists(),
        "hidden health scan should not persist suppression"
    );

    let shown = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store_str.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );

    assert!(shown.contains("Health check"), "{shown}");
    assert!(shown.contains("Suggested prompts"), "{shown}");
}

#[test]
fn raw_cli_startup_health_prompt_ghost_tab_fills_first_suggestion() {
    let cwd = temp_shell_home("startup-health-ghost-tab");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("NO_COLOR", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_RENDER", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[
            (
                " › Analyze memory pressure and identify top consumers",
                b"\t\n",
            ),
            ("Received shell prompt request", b"exit\n"),
        ],
    );

    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(
        output.contains("Analyze memory pressure and identify top consumers"),
        "{output}"
    );
    let first_prompt = first_health_prompt(&output).expect("first health prompt");
    let compact = compact_without_box_chars(&output);
    assert!(
        compact.contains(&format!("Received shell prompt request: {first_prompt}")),
        "{output}"
    );
    assert!(
        compact.contains("Runtime context hints visible to Agent"),
        "{output}"
    );
    assert!(compact.contains("health_scan"), "{output}");
    assert!(compact.contains("scan_id=health-"), "{output}");
    assert!(compact.contains("bounded_facts_only=true"), "{output}");
    assert!(
        !output.contains("command not found: Analyze memory pressure"),
        "{output}"
    );
}

#[test]
fn raw_cli_startup_health_prompt_ghost_tab_only_does_not_submit() {
    let cwd = temp_shell_home("startup-health-ghost-tab-only");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("NO_COLOR", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_RENDER", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        vec![
            (b"\t".to_vec(), Duration::from_millis(1400)),
            (vec![0x15], Duration::from_millis(300)),
            (b"exit\n".to_vec(), Duration::from_millis(700)),
        ],
    );

    assert!(first_health_prompt(&output).is_some(), "{output}");
    assert!(
        !output.contains("Received shell prompt request:"),
        "{output}"
    );
}

#[test]
fn raw_cli_startup_health_shift_tab_cycles_then_enter_submits_active_prompt() {
    let cwd = temp_shell_home("startup-health-shift-tab-enter");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[
            (
                " › Analyze memory pressure and identify top consumers",
                b"\x1b[Z",
            ),
            (
                " › Check whether swap pressure is active and which processes",
                b"\n",
            ),
            (
                "Received shell prompt request: Check whether swap pressure is active",
                b"exit\n",
            ),
        ],
    );

    assert_eq!(output.matches("[Health]").count(), 3, "{output}");
    assert!(
        output.contains("\x1b[2m › Check whether swap pressure"),
        "{output}"
    );
    assert!(
        compact_without_box_chars(&output).contains(
            "Received shell prompt request: Check whether swap pressure is active and which processes"
        ),
        "{output}"
    );
    assert!(!output.contains("command not found"), "{output}");
}

#[test]
fn raw_cli_startup_health_prompt_selection_respects_terminal_capability_not_color() {
    for (name, extra_env) in [
        ("dumb", vec![("TERM", "dumb")]),
        (
            "plain",
            vec![("TERM", "xterm-256color"), ("COSH_SHELL_RENDER", "plain")],
        ),
        (
            "no-color",
            vec![("TERM", "xterm-256color"), ("NO_COLOR", "1")],
        ),
    ] {
        let cwd = temp_shell_home(&format!("startup-health-ghost-disabled-{name}"));
        let suppression_store = cwd.join("health-suppression");
        let suppression_store = suppression_store.to_string_lossy().into_owned();
        let mut env = vec![
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ISOLATED", "0"),
        ];
        env.extend(extra_env);
        let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
            "fake",
            &[],
            &env,
            &cwd,
            vec![
                (b"\t\n".to_vec(), Duration::from_millis(1400)),
                (b"exit\n".to_vec(), Duration::from_millis(700)),
            ],
        );

        if name == "dumb" {
            assert!(!output.contains("Suggested prompts"), "{name}\n{output}");
            assert!(first_health_prompt(&output).is_none(), "{name}\n{output}");
            assert!(
                !output.contains("Received shell prompt request:"),
                "{name}\n{output}"
            );
        } else {
            assert!(output.contains("Suggested prompts"), "{name}\n{output}");
            assert!(first_health_prompt(&output).is_some(), "{name}\n{output}");
            assert!(
                output.contains("Received shell prompt request:"),
                "{name}\n{output}"
            );
        }
    }
}

#[test]
fn raw_cli_startup_health_prompt_ghost_does_not_override_manual_input() {
    let cwd = temp_shell_home("startup-health-ghost-manual");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("NO_COLOR", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_RENDER", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        vec![
            (b"echo manual\n".to_vec(), Duration::from_millis(1400)),
            (b"exit\n".to_vec(), Duration::from_millis(700)),
        ],
    );

    assert!(first_health_prompt(&output).is_some(), "{output}");
    assert!(output.contains("echo manual"), "{output}");
    assert!(output.contains("manual"), "{output}");
    assert!(
        !output.contains("Received shell prompt request: Analyze memory pressure"),
        "{output}"
    );
    assert!(
        !output.contains("command not found: Analyze memory pressure"),
        "{output}"
    );
}

#[test]
fn raw_cli_startup_health_context_is_not_attached_to_plain_agent_request() {
    let cwd = temp_shell_home("startup-health-context");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
        &cwd,
        vec![
            (
                b"please show context\n".to_vec(),
                Duration::from_millis(1_000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(
        output.contains("Runtime context hints visible to Agent"),
        "{output}"
    );
    let compact = compact_terminal_words(&output);
    assert!(compact.contains("Runtime context hints visible to Agent: <none>"));
    assert!(!compact.contains("health_scan"), "{output}");
    assert!(!compact.contains("scan_id=health-"), "{output}");
    assert!(!compact.contains("bounded_facts_only=true"), "{output}");
    assert!(!output.contains("journalctl -k"), "{output}");
    let context_tail = output
        .split("Runtime context hints visible to Agent")
        .nth(1)
        .unwrap_or("");
    let context_card = context_tail.split('╰').next().unwrap_or(context_tail);
    assert!(!context_card.contains("/tmp/cosh"), "{output}");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn raw_cli_startup_health_live_does_not_render_on_non_linux_by_default() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(!output.contains("Health check"), "{output}");
    assert!(!output.contains("Health:"), "{output}");
    assert!(!output.contains("platform unsupported"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_startup_health_live_does_not_block_shell_on_linux_by_default() {
    let output = run_raw_cli_with_env(
        "fake",
        "echo after-health-startup\nexit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("after-health-startup"), "{output}");
    assert!(!output.contains("platform unsupported"), "{output}");
}

#[test]
fn raw_cli_startup_hooks_use_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_STARTUP_HOOKS", "1"),
            ("COSH_SHELL_LANG", "zh-CN"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(
        output.contains("启动 hooks: 内置只读检查已完成"),
        "{output}"
    );
    assert!(output.contains("启动检查结果"), "{output}");
    assert!(
        output.contains("检测到 Cargo.toml Rust 项目")
            || output.contains("内置只读检查未发现启动项"),
        "{output}"
    );
    assert!(
        output.contains("cosh-shell 只检查了轻量启动上下文"),
        "{output}"
    );
    for label in [
        "Startup hooks:",
        "Startup findings",
        "Rust project detected",
        "No startup findings from built-in read-only checks",
        "only inspected lightweight startup context",
    ] {
        assert!(
            !output.contains(label),
            "startup English UI label leaked: {label}\n{output}"
        );
    }
}

#[test]
fn raw_cli_startup_hooks_no_findings_use_zh_language_env() {
    let cwd = temp_shell_home("startup-hooks-no-findings");
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_STARTUP_HOOKS", "1"),
            ("COSH_SHELL_LANG", "zh-CN"),
            ("TERM", "xterm-256color"),
        ],
        &cwd,
        vec![(b"exit\n".to_vec(), Duration::ZERO)],
    );

    assert!(
        output.contains("启动 hooks: 内置只读检查已完成"),
        "{output}"
    );
    assert!(output.contains("启动检查结果"), "{output}");
    assert!(output.contains("内置只读检查未发现启动项"), "{output}");
    assert!(
        !output.contains("No startup findings from built-in read-only checks"),
        "{output}"
    );
}

#[test]
fn raw_cli_default_agent_mode_defers_safe_fallback_tool() {
    let home = temp_shell_home("default-agent-auto");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? request tool approval\n"),
            ("Deferred req-1", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Approval: auto"), "{output}");
    assert!(output.contains("Analysis: smart"), "{output}");
    assert!(output.contains("Deferred req-1"), "{output}");
    assert!(output.contains("$ git status"), "{output}");
    assert!(!output.contains("Approval req-"), "{output}");
    assert!(!output.contains("[ Allow once ]"), "{output}");
    assert!(!output.contains("Approved req-1"), "{output}");
    assert!(!output.contains("Auto-approved req-1"), "{output}");
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        !output.contains("evidence: ShellCommandCompleted"),
        "{output}"
    );
}

#[test]
fn raw_cli_raw_run_without_adapter_uses_cosh_core_default_adapter() {
    let home = temp_shell_home("cosh-core-default-adapter");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
case "$init" in
  *'"subtype":"initialize"'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-default","is_error":true,"result":"missing initialize"}'; exit 1 ;;
esac
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-default","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-default-adapter-smoke*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-default","message":{"content":[{"type":"text","text":"Cosh-core default adapter reached via implicit raw."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-default","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-default","is_error":true,"result":"unexpected prompt"}'
"#,
    );

    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_default_with_args_env_and_delayed_input(
        &["--run"],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
        ],
        vec![
            (
                b"?? cosh-core-default-adapter-smoke\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"/debug session\n".to_vec(), Duration::from_millis(500)),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Adapter: cosh-core"), "{output}");
    assert!(
        output.contains("Cosh-core default adapter reached via implicit raw."),
        "{output}"
    );
    assert!(output.contains("provider invocation:"), "{output}");
    assert!(
        output.contains("cosh-raw-cli-cosh-core-default-adapter"),
        "{output}"
    );
    assert!(output.contains("/bin/cosh-core"), "{output}");
    assert!(!output.contains("Adapter: fake"), "{output}");
    assert!(!output.contains("unexpected prompt"), "{output}");
    assert!(!output.contains("failed to run cosh-core"), "{output}");
}

#[test]
fn raw_cli_startup_hooks_render_markdown_findings_without_running_commands() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_STARTUP_HOOKS", "1"),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(
        output.contains("Startup hooks: built-in read-only checks completed"),
        "{output}"
    );
    assert!(output.contains("Startup findings"), "{output}");
    assert!(
        output.contains("Rust project detected from Cargo.toml")
            || output.contains("No startup findings from built-in read-only checks"),
        "{output}"
    );
    assert!(
        output.contains("cosh-shell only inspected lightweight startup context"),
        "{output}"
    );
    assert!(
        !output.contains("Read-only startup checks."),
        "hook findings should be inline, not a separate panel: {output}"
    );
    assert!(!output.contains("No command ran."), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_startup_banner_reports_selected_zsh_shell() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_and_env(
        "fake",
        &["--shell", "zsh"],
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("Shell: zsh"), "{output}");
    assert!(!output.contains("Shell: bash"), "{output}");
    assert!(!output.contains("zsh: command not found"), "{output}");
}

#[test]
fn raw_cli_shell_arg_can_select_zsh_raw_host() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_and_env(
        "fake",
        &["--shell", "zsh"],
        "echo zsh-cli:$ZSH_VERSION\nexit\n",
        &[("SHELL", "/bin/bash"), ("TERM", "xterm-256color")],
    );

    assert!(output.contains("zsh-cli:5"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("\x1b]1337;COSH;"), "{output}");
}

#[test]
fn raw_cli_unsupported_shell_reports_error_without_starting_bash() {
    assert_raw_cli_rejects_shell_args(
        &["raw", "fake", "--shell", "fish"],
        "unsupported raw shell: fish; supported shells: bash, zsh",
    );
}

#[test]
fn raw_cli_adapter_failure_keeps_shell_usable() {
    let output = run_raw_cli_with_input(
        "fake",
        "?? backend unavailable\n\
         echo after-backend-unavailable\n\
         exit 0\n",
    );

    assert!(output.contains("fake backend unavailable"), "{output}");
    assert!(output.contains("after-backend-unavailable"), "{output}");
    assert!(!output.contains("bash: ??"), "{output}");
    assert!(
        !output.contains("The command ?? backend unavailable failed"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_backend_unavailable_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "?? backend unavailable\n\
         echo after-backend-unavailable\n\
         exit 0\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("正在思考..."), "{output}");
    assert!(output.contains("Agent 回复"), "{output}");
    assert!(output.contains("治理"), "{output}");
    assert!(output.contains("fake backend unavailable"), "{output}");
    assert!(output.contains("after-backend-unavailable"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("Agent response:"), "{output}");
    assert!(!output.contains("bash: ??"), "{output}");
    assert!(
        !output.contains("The command ?? backend unavailable failed"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_adapter_error_keeps_shell_usable() {
    let output = run_raw_cli_with_input(
        "fake",
        "?? adapter crash\n\
         echo after-adapter-crash\n\
         exit 0\n",
    );

    assert!(output.contains("fake adapter crashed"), "{output}");
    assert!(output.contains("after-adapter-crash"), "{output}");
    assert!(!output.contains("bash: ??"), "{output}");
    assert!(
        !output.contains("The command ?? adapter crash failed"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_missing_shell_arg_reports_error_without_starting_bash() {
    assert_raw_cli_rejects_shell_args(
        &["raw", "fake", "--shell"],
        "missing value for --shell; supported shells: bash, zsh",
    );
}

fn first_health_prompt(output: &str) -> Option<String> {
    strip_ansi_escape(output)
        .lines()
        .find_map(|line| {
            let (_, prompt) = line.split_once("[Health]")?;
            Some(prompt.trim().trim_end_matches('│').trim().to_string())
        })
        .filter(|prompt| !prompt.is_empty())
}

fn compact_without_box_chars(output: &str) -> String {
    strip_ansi_escape(output)
        .chars()
        .map(|ch| {
            if matches!(ch, '│' | '╭' | '╰' | '─' | '╮' | '╯') {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn box_line_widths(output: &str) -> Vec<usize> {
    strip_ansi_escape(output)
        .replace('\r', "\n")
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_end();
            if trimmed.starts_with('╭') || trimmed.starts_with('╰') || trimmed.starts_with('│')
            {
                Some(terminal_display_width(trimmed))
            } else {
                None
            }
        })
        .collect()
}

fn terminal_display_width(line: &str) -> usize {
    Span::raw(line).width()
}

fn assert_raw_cli_rejects_shell_args(args: &[&str], expected: &str) {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run shell selection error case");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains(expected),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("cosh-osc$"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stderr.contains("bash:"),
        "stdout={stdout}\nstderr={stderr}"
    );
}
