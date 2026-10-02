use super::*;

#[test]
fn diagnostics_export_collects_sources_without_leaking_secrets() {
    let home = temp_shell_home("diagnostics-export");
    let state = home.join(".copilot-shell");
    let logs = state.join("logs");
    fs::create_dir_all(&logs).expect("create log directory");
    fs::write(
        logs.join("cosh-shell.log.current"),
        "request token=diagnostic-secret-token\n",
    )
    .expect("write diagnostic log");
    fs::write(
        state.join("audit-events.jsonl"),
        "{\"authorization\":\"Bearer diagnostic-secret-auth\"}\n",
    )
    .expect("write diagnostic events");
    fs::write(
        state.join("last-crash.log"),
        "password: diagnostic-secret-password\n",
    )
    .expect("write crash summary");
    let output = home.join("diagnostic.json");

    let command_output = Command::new(env!("CARGO_BIN_EXE_cosh-shell"))
        .args(["diagnostics", "export", "--output"])
        .arg(&output)
        .env("HOME", &home)
        .env("COSH_SHELL_HEALTH_SCAN", "fixture:linux-healthy")
        .output()
        .expect("run diagnostics export");
    assert!(
        command_output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&command_output.stderr)
    );

    let content = fs::read_to_string(&output).expect("read diagnostic bundle");
    let bundle: serde_json::Value =
        serde_json::from_str(&content).expect("parse diagnostic bundle");
    assert_eq!(bundle["format"], "cosh-diagnostic-bundle");
    assert_eq!(bundle["version"], 1);
    assert_eq!(bundle["sources"]["health"]["overall_severity"], "ok");
    assert!(bundle["sources"]["health"]["findings"].is_array());
    assert!(bundle["sources"]["health"]["unavailable"].is_array());
    assert!(bundle["sources"]["health"]["try_items"].is_array());
    assert!(!content.contains("diagnostic-secret-token"));
    assert!(!content.contains("diagnostic-secret-auth"));
    assert!(!content.contains("diagnostic-secret-password"));
    assert!(content.contains("<redacted>"));
    assert_eq!(
        fs::metadata(&output)
            .expect("diagnostic metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let _ = fs::remove_dir_all(home);
}

/// Regression for issue #1744: `diagnostics export` used to call the bare
/// resource scan, so the bundle health section missed the environment
/// collector checks (provider/config/hooks/pty/permissions) that the
/// `cosh-shell doctor` CLI reports. Both entry points must share the doctor
/// engine and produce the same `checks` coverage. The resource scan is
/// disabled here so the comparison is deterministic and exercises exactly the
/// env collector half that the bundle previously dropped.
///
/// Format contract: the doctor CLI has no structured output, so the parity
/// comparison reads the plain-text `checks: <name>, <name>` line emitted by
/// `format_doctor_report_plain`; pinning `COSH_SHELL_LANG=en-US` (which
/// overrides locale detection) plus `LC_ALL=en_US.UTF-8` keeps that label
/// stable under i18n. If the doctor plain format changes, update this parser
/// together with the renderer.
#[test]
fn diagnostics_export_health_checks_match_doctor_cli() {
    let home = temp_shell_home("diagnostics-doctor-parity");
    let output = home.join("bundle.json");

    let export_output = Command::new(env!("CARGO_BIN_EXE_cosh-shell"))
        .args(["diagnostics", "export", "--output"])
        .arg(&output)
        .env("HOME", &home)
        .env("COSH_SHELL_HEALTH_SCAN", "disabled")
        .output()
        .expect("run diagnostics export");
    assert!(
        export_output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&export_output.stderr)
    );
    let content = fs::read_to_string(&output).expect("read diagnostic bundle");
    let bundle: serde_json::Value =
        serde_json::from_str(&content).expect("parse diagnostic bundle");
    let mut bundle_checks: Vec<String> = bundle["sources"]["health"]["checks_done"]
        .as_array()
        .expect("bundle health checks_done array")
        .iter()
        .map(|value| value.as_str().expect("check name").to_string())
        .collect();
    bundle_checks.sort();
    bundle_checks.dedup();

    let doctor_output = Command::new(env!("CARGO_BIN_EXE_cosh-shell"))
        .arg("doctor")
        .env("HOME", &home)
        .env("COSH_SHELL_HEALTH_SCAN", "disabled")
        .env("COSH_SHELL_LANG", "en-US")
        .env("LC_ALL", "en_US.UTF-8")
        .output()
        .expect("run cosh-shell doctor");
    // Doctor's exit contract is 0=healthy, 1=warning, 2=error; a fresh HOME
    // legitimately reports warnings, so only reject crash/signal exits.
    assert!(
        matches!(doctor_output.status.code(), Some(0..=2)),
        "doctor exited abnormally: status={:?} stderr={}",
        doctor_output.status,
        String::from_utf8_lossy(&doctor_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&doctor_output.stdout);
    let doctor_checks: Vec<String> = stdout
        .lines()
        .find_map(|line| line.strip_prefix("checks: "))
        .unwrap_or_else(|| panic!("doctor checks line missing: {stdout}"))
        .split(", ")
        .map(str::to_string)
        .collect();

    assert_eq!(bundle_checks, doctor_checks, "doctor stdout={stdout}");
    for check in ["provider", "config", "hooks", "pty", "permissions"] {
        assert!(
            bundle_checks.iter().any(|done| done == check),
            "bundle health section missing env check {check}: {bundle_checks:?}"
        );
    }

    let _ = fs::remove_dir_all(home);
}
