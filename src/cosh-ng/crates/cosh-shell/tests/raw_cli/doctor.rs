use super::*;

/// Run `cosh-shell doctor` against a deterministic health fixture and return
/// (exit_code, stdout, stderr). Fixture mode makes the resource severity
/// dominate and skips the live environment collectors, so results are stable.
fn run_doctor_fixture(fixture: &str) -> (Option<i32>, String, String) {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .arg("doctor")
        .env("COSH_SHELL_HEALTH_SCAN", format!("fixture:{fixture}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run cosh-shell doctor");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn doctor_healthy_environment_exits_zero() {
    let (code, stdout, stderr) = run_doctor_fixture("linux-healthy");
    assert_eq!(code, Some(0), "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("status: healthy"), "stdout={stdout}");
    assert!(stdout.contains("all checks passed"), "stdout={stdout}");
}

#[test]
fn doctor_degraded_environment_exits_one() {
    let (code, stdout, stderr) = run_doctor_fixture("linux-degraded");
    assert_eq!(code, Some(1), "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("status: warning"), "stdout={stdout}");
}

#[test]
fn doctor_partially_unavailable_environment_exits_one() {
    let (code, stdout, stderr) = run_doctor_fixture("linux-partial");
    assert_eq!(code, Some(1), "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("status: warning"), "stdout={stdout}");
    // Checks that could not run are surfaced without blocking the rest.
    assert!(stdout.contains("[unavailable]"), "stdout={stdout}");
}

#[test]
fn doctor_failed_environment_exits_two() {
    let (code, stdout, stderr) = run_doctor_fixture("linux-critical");
    assert_eq!(code, Some(2), "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("status: error"), "stdout={stdout}");
    assert!(stdout.contains("[critical]"), "stdout={stdout}");
}

#[test]
fn slash_health_renders_health_card() {
    let output = run_raw_cli_with_env(
        "fake",
        "/health\nexit\n",
        &[("COSH_SHELL_HEALTH_SCAN", "fixture:linux-critical")],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("critical"), "{output}");
    assert!(!output.contains("command not found"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
}

/// After the user `cd`s into a project carrying untrusted `.cosh/hooks`, the
/// `/health` hook check must evaluate that child-shell directory (not the
/// parent launch cwd) and surface the project-hooks remediation. Live mode is
/// used so the environment collectors actually run against the new cwd.
#[test]
fn slash_health_uses_child_shell_cwd_for_hook_checks() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let project = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-hookcwd-{}-{}",
        std::process::id(),
        unique
    ));
    let hooks_dir = project.join(".cosh/hooks");
    std::fs::create_dir_all(&hooks_dir).expect("create project .cosh/hooks");
    std::fs::write(hooks_dir.join("check.sh"), "#!/bin/sh\nexit 0\n").expect("write project hook");

    let input = format!("cd {}\n/health\nexit\n", project.display());
    let output = run_raw_cli_with_env("fake", &input, &[]);

    // The English remediation for untrusted project hooks references the path
    // discovered from the child shell cwd; if /health used the parent launch
    // cwd it would not find this project's hooks.
    assert!(
        output.contains("review and trust project hooks under"),
        "{output}"
    );
    assert!(output.contains("Health check"), "{output}");

    let _ = std::fs::remove_dir_all(&project);
}

/// Builds a project whose `.cosh/hooks` holds the two things a project can create
/// at will to attack a health check: a top-level FIFO, which blocks a read-only
/// `open` until a writer appears, and an executable hook far over the loader's
/// 4 MiB per-file ceiling (`MAX_PROJECT_HOOK_FINGERPRINT_BYTES`), which an unbounded
/// read would buffer whole. Neither file is a valid hook, and both are sparse or
/// empty, so the fixture costs almost nothing on disk.
#[cfg(unix)]
fn project_with_hostile_hook_files(label: &str) -> std::path::PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let project = std::env::temp_dir().join(format!(
        "cosh-shell-doctor-{label}-{}-{unique}",
        std::process::id()
    ));
    let hooks_dir = project.join(".cosh/hooks");
    std::fs::create_dir_all(&hooks_dir).expect("create project .cosh/hooks");

    let fifo = hooks_dir.join("block");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo must succeed for this fixture");
    use std::os::unix::fs::FileTypeExt;
    assert!(
        std::fs::symlink_metadata(&fifo)
            .map(|metadata| metadata.file_type().is_fifo())
            .unwrap_or(false),
        "the fixture must actually contain a FIFO"
    );

    // A valid header keeps it a candidate for loading, so the size gate is what
    // refuses it rather than the header parser.
    let huge = hooks_dir.join("huge.sh");
    let mut file = std::fs::File::create(&huge).expect("create oversized hook");
    file.write_all(b"#!/bin/sh\n# cosh-hook: huge\n# match-commands: echo\n")
        .expect("write header");
    file.set_len(8 * 1024 * 1024)
        .expect("extend past the ceiling");
    drop(file);
    std::fs::set_permissions(&huge, std::fs::Permissions::from_mode(0o755))
        .expect("make it executable");
    project
}

/// Runs `cosh-shell doctor` live — no `COSH_SHELL_HEALTH_SCAN` fixture, so the
/// environment collectors really run against `current_dir` — and fails the test
/// instead of hanging forever if the process does not finish.
#[cfg(unix)]
fn run_live_doctor_in(
    current_dir: &std::path::Path,
    envs: &[(&str, &str)],
) -> (Option<i32>, String) {
    let binary = env!("CARGO_BIN_EXE_cosh-shell").to_string();
    let directory = current_dir.to_path_buf();
    let envs: Vec<_> = envs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = raw_cli_command(&binary)
            .arg("doctor")
            .current_dir(&directory)
            .envs(envs)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        let _ = done_tx.send(output.map(|output| {
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            )
        }));
    });
    let finished = done_rx
        .recv_timeout(Duration::from_secs(90))
        .expect("cosh-shell doctor did not finish: a file the project supplies blocked the health collector");
    finished.expect("cosh-shell doctor could not be run")
}

/// The health collector now runs the hook loader to fingerprint what a session
/// would pin, which makes it a new entry point for a project to block. Before the
/// loader grew its type and size gates, a top-level FIFO here waited for a writer
/// that never comes and `timeout 3s cosh-shell doctor` exited 124.
#[cfg(unix)]
#[test]
fn doctor_completes_on_a_project_with_a_fifo_and_an_oversized_hook() {
    let project = project_with_hostile_hook_files("live");
    let (code, stdout) = run_live_doctor_in(&project, &[]);

    assert!(
        code.is_some(),
        "doctor was killed by a signal rather than reporting a health status"
    );
    assert!(
        stdout.contains("status:"),
        "doctor must still report a health status, got {stdout}"
    );
    let _ = std::fs::remove_dir_all(&project);
}

/// `/health` runs the same collector inside a live shell, so it needs the same
/// guarantee: a project's hook directory must not be able to wedge the session.
#[cfg(unix)]
#[test]
fn slash_health_completes_on_a_project_with_a_fifo_and_an_oversized_hook() {
    let project = project_with_hostile_hook_files("slash");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let directory = project.clone();
    std::thread::spawn(move || {
        let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
            "fake",
            &[],
            &[],
            &directory,
            vec![
                (b"/health\n".to_vec(), Duration::ZERO),
                (b"exit\n".to_vec(), Duration::from_millis(3_000)),
            ],
        );
        let _ = done_tx.send(output);
    });
    let output = done_rx
        .recv_timeout(Duration::from_secs(120))
        .expect("/health did not finish: a file the project supplies blocked the health collector");

    assert!(
        output.contains("Health check"),
        "the health card must still render, got {output}"
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[cfg(unix)]
#[test]
fn doctor_and_slash_health_withhold_consent_for_over_budget_invalid_candidates() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("project");
    let hooks = project.join(".cosh/hooks");
    fs::create_dir_all(&hooks).unwrap();
    write_executable(
        &hooks.join("valid.sh"),
        "#!/bin/sh\n# cosh-hook: valid\n# match-commands: echo\n",
    );
    let store = home.path().join("trust-store");
    let home_str = home.path().to_str().unwrap();
    let store_str = store.to_str().unwrap();
    let envs = [
        ("HOME", home_str),
        ("COSH_SHELL_PROJECT_TRUST_STORE", store_str),
    ];
    let consent = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &envs,
        &project,
        vec![
            (b"/hooks trust-project\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(2500)),
        ],
    );
    assert!(store.exists(), "{consent}");
    let recorded = fs::read_to_string(&store).unwrap();
    let (_, positive) = run_live_doctor_in(&project, &envs);
    assert!(
        !positive.contains("review and trust project hooks under"),
        "{positive}"
    );
    for (label, count, size) in [("count", 65, 1), ("bytes", 2, 3 * 1024 * 1024)] {
        for index in 0..count {
            let path = hooks.join(format!("invalid-{index}"));
            write_executable(&path, &"x".repeat(size));
        }
        let (code, report) = run_live_doctor_in(&project, &envs);
        assert!(code.is_some(), "{label}: doctor killed");
        assert!(
            report.contains("review and trust project hooks under"),
            "{label}: {report}"
        );
        let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
            "fake",
            &[],
            &envs,
            &project,
            vec![
                (b"/health\n".to_vec(), Duration::ZERO),
                (
                    b"/hooks trust-project\n".to_vec(),
                    Duration::from_millis(2500),
                ),
                (b"exit\n".to_vec(), Duration::from_millis(2500)),
            ],
        );
        assert!(output.contains("Health check"), "{label}: {output}");
        assert!(
            output.contains("review and trust project hooks under"),
            "{label}: {output}"
        );
        assert!(
            output.contains("No trust state changed"),
            "{label}: {output}"
        );
        assert_eq!(fs::read_to_string(&store).unwrap(), recorded);
        for index in 0..count {
            fs::remove_file(hooks.join(format!("invalid-{index}"))).unwrap();
        }
    }
}
