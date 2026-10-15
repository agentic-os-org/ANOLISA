use super::*;

#[test]
fn raw_cli_audit_status_is_bounded_and_restores_prompt() {
    let directory = std::env::temp_dir().join(format!(
        "cosh-shell-raw-audit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir(&directory).expect("create audit fixture directory");
    let cli = directory.join("cosh-cli");
    fs::write(
        &cli,
        "#!/bin/sh\nprintf '%s' '{\"ok\":true,\"data\":{\"mode\":\"best_effort\",\"root_source\":\"environment\"}}'\n",
    )
    .expect("write cosh-cli fixture");
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).expect("mode fixture");

    let output = run_raw_cli_with_env(
        "fake",
        "/audit status\necho after-audit-status\nexit\n",
        &[("COSH_CLI_BIN", cli.to_str().expect("utf8 fixture path"))],
    );

    assert!(output.contains("Audit"), "{output}");
    assert!(output.contains("best_effort"), "{output}");
    assert!(output.contains("after-audit-status"), "{output}");
    assert!(!output.contains("bash: /audit"), "{output}");
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn raw_cli_audit_export_forwards_a_destination_containing_spaces() {
    let directory = std::env::temp_dir().join(format!(
        "cosh-shell-raw-audit-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir(&directory).expect("create audit fixture directory");
    let argv = directory.join("argv");
    let cli = directory.join("cosh-cli");
    fs::write(
        &cli,
        format!(
            "#!/bin/sh\nfor argument in \"$@\"; do printf '%s\\n' \"$argument\" >>'{}'; done\nprintf '%s' '{{\"ok\":true,\"data\":{{\"bundle\":\"written\"}}}}'\n",
            argv.display()
        ),
    )
    .expect("write cosh-cli fixture");
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).expect("mode fixture");

    let output = run_raw_cli_with_env(
        "fake",
        "/audit export current /tmp/my dir\necho after-audit-export\nexit\n",
        &[("COSH_CLI_BIN", cli.to_str().expect("utf8 fixture path"))],
    );

    let forwarded = fs::read_to_string(&argv).expect("cosh-cli fixture recorded argv");
    let forwarded = forwarded.lines().collect::<Vec<_>>();
    assert_eq!(forwarded.len(), 6, "{forwarded:?}");
    assert_eq!(
        forwarded[..4],
        ["audit", "export", "--output", "/tmp/my dir"]
    );
    assert_eq!(forwarded[4], "--identity");
    assert!(output.contains("written"), "{output}");
    assert!(output.contains("after-audit-export"), "{output}");
    let _ = fs::remove_dir_all(directory);
}

/// S02 end to end: with the audit recorder in `Required` mode and a genuinely
/// unusable audit root, a provider tool request must never reach the executor,
/// while ordinary Bash keeps working. Once the root is usable again the very
/// same request executes.
///
/// The write failure is injected by making the audit root path a *regular file*,
/// so the writer's `openat(O_DIRECTORY)` fails with ENOTDIR. Unlike `chmod 000`
/// this is not a DAC check, so running the suite as root cannot bypass it.
///
/// Effect counting does not rely on the command being unsafe: the deterministic
/// Core writes `dispatched.marker` only when it actually receives a
/// `host_executed_shell` result, so the marker is the effect record.
///
/// Scope, so the two S02 cases are not read as covering the same site: the
/// provider here is cosh-core and the request is a control permission, which
/// makes it Core-owned, so this case exercises the host-execution audit boundary
/// (`append_governed` on `tool.execution.started`). The approval-resolution
/// branch is Core-owned-skipped here and is pinned instead by
/// `journal::audit::tests::required_mode_real_write_failure_blocks_then_recovers`.
/// Interactive human approval is a third site that neither case reaches.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn raw_cli_required_audit_write_failure_blocks_dispatch_and_recovers() {
    let system = audit_system_config::AuditSystemConfig::new()
        .expect("isolated audit fixture requires Linux/glibc, cc, and dynamic linking");
    required_audit_write_failure_blocks_dispatch_and_recovers(&system, "required");
    system.write("[audit]\nmode = \"required\"\n").unwrap();
    required_audit_write_failure_blocks_dispatch_and_recovers(&system, "best_effort");
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn required_audit_write_failure_blocks_dispatch_and_recovers(
    system: &audit_system_config::AuditSystemConfig,
    user_mode: &str,
) {
    let home = temp_shell_home("s02-required-audit");
    write_cosh_config(&home, &format!("[audit]\nmode = \"{user_mode}\"\n"));

    let state_dir = home.join(".local/state/cosh");
    fs::create_dir_all(&state_dir).unwrap();
    let audit_root = state_dir.join("audit");
    // Blocker: a regular file where the audit directory must be.
    fs::write(&audit_root, b"not a directory").unwrap();

    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    let marker = home.join("dispatched.marker");
    let marker_str = marker.to_string_lossy().to_string();
    write_executable(
        &cosh_core_path,
        &format!(
            r#"#!/bin/bash
read -r init
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"init-1","response":{{"subtype":"initialize","capabilities":{{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}}}}}'
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"sess-s02","model":"cosh-core-test"}}'
read -r user_message
case "$user_message" in
  *s02-required-audit-trigger*)
    printf '%s\n' '{{"type":"control_request","request_id":"ctrl-s02","request":{{"subtype":"can_use_tool","tool_name":"shell","input":{{"command":"df -h"}},"tool_use_id":"toolu-s02"}}}}'
    response=""
    while IFS= read -r -t 20 line; do
      case "$line" in
        *'"type":"approval_receipt"'*) continue ;;
        *) response="$line"; break ;;
      esac
    done
    case "$response" in
      *'"behavior":"host_executed_shell"'*)
        touch '{marker_str}'
        printf '%s\n' '{{"type":"assistant","session_id":"sess-s02","message":{{"content":[{{"type":"text","text":"S02 DISPATCHED"}}]}}}}'
        ;;
      *)
        printf '%s\n' '{{"type":"assistant","session_id":"sess-s02","message":{{"content":[{{"type":"text","text":"S02 NOT DISPATCHED"}}]}}}}'
        ;;
    esac
    printf '%s\n' '{{"type":"result","subtype":"success","session_id":"sess-s02","is_error":false,"result":"done"}}'
    exit 0
    ;;
esac
printf '%s\n' '{{"type":"result","subtype":"success","session_id":"sess-s02","is_error":false,"result":"ignored"}}'
"#
        ),
    );

    let home_str = home.to_string_lossy().to_string();
    let core_str = cosh_core_path.to_string_lossy().to_string();
    let preload = system.path("audit-config.so");
    let config = system.path("config.toml");
    let read_marker = system.path("read.marker");
    let scenario = |trigger: &str| {
        system.reset_read_marker();
        let output = run_raw_cli_serial_with_args_env_and_delayed_input(
            "cosh-core",
            &[],
            &[
                ("HOME", &home_str),
                ("COSH_CORE_PATH", &core_str),
                ("COSH_AUDIT_DIR", RAW_CLI_UNSET_ENV),
                ("XDG_STATE_HOME", RAW_CLI_UNSET_ENV),
                ("LD_PRELOAD", preload.to_str().unwrap()),
                ("COSH_TEST_AUDIT_SYSTEM_CONFIG", config.to_str().unwrap()),
                (
                    "COSH_TEST_AUDIT_SYSTEM_READ_MARKER",
                    read_marker.to_str().unwrap(),
                ),
            ],
            vec![
                (b"/mode approval auto\n".to_vec(), Duration::ZERO),
                (
                    format!("?? {trigger}\n").into_bytes(),
                    Duration::from_millis(500),
                ),
                (
                    b"echo S02_ORDINARY_BASH_OK\n".to_vec(),
                    Duration::from_millis(2_500),
                ),
                (b"exit 0\n".to_vec(), Duration::from_millis(4_000)),
            ],
        );
        system.assert_read();
        output
    };

    // --- Phase 1: audit unwritable -> governed dispatch must fail closed ---
    let blocked = scenario("s02-required-audit-trigger");
    assert!(
        !marker.exists(),
        "effect count must be zero: executor ran despite the required audit write failure\n{blocked}"
    );
    assert!(
        !blocked.contains("Filesystem"),
        "df -h must not have executed\n{blocked}"
    );
    assert!(
        blocked.contains("S02_ORDINARY_BASH_OK"),
        "ordinary Bash must stay usable while audit is failing\n{blocked}"
    );

    // --- Phase 2: recovery -> the same request must dispatch (positive control) ---
    fs::remove_file(&audit_root).unwrap();
    fs::create_dir_all(&audit_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&audit_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let recovered = scenario("s02-required-audit-trigger");
    assert!(
        marker.exists(),
        "positive control: the same request must dispatch once audit is writable\n{recovered}"
    );
    assert!(
        recovered.contains("Filesystem"),
        "df -h must really execute after recovery\n{recovered}"
    );
    assert!(
        recovered.contains("S02_ORDINARY_BASH_OK"),
        "ordinary Bash must still work after recovery\n{recovered}"
    );

    let _ = fs::remove_dir_all(&home);
}
