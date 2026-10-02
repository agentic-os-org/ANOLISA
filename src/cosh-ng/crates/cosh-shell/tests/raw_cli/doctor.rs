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
