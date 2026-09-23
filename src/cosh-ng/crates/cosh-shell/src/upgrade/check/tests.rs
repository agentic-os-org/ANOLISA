use std::sync::mpsc;

use super::*;

#[test]
fn anolisa_preview_compares_target_with_running_version() {
    let json = br#"{"ok":true,"data":{"package":"cosh-ng","to_version":"1.3.0","updated":false,"plan":["replace"]}}"#;
    assert_eq!(
        parse_anolisa_output(json, "cosh-ng", "1.2.3", AnolisaScope::User),
        Some(UpgradeVerdict::Upgrade(UpgradeNotice {
            package: "cosh-ng".to_string(),
            current: "1.2.3".to_string(),
            latest: "1.3.0".to_string(),
            command: "anolisa update cosh-ng".to_string(),
        }))
    );
}

#[test]
fn anolisa_system_scope_notice_names_the_system_install_mode() {
    let json = br#"{"ok":true,"data":{"to_version":"1.3.0","plan":["replace"]}}"#;
    assert!(matches!(
        parse_anolisa_output(json, "cosh-ng", "1.2.3", AnolisaScope::System),
        Some(UpgradeVerdict::Upgrade(UpgradeNotice { command, .. }))
            if command == "sudo anolisa --install-mode system update cosh-ng"
    ));
}

#[test]
fn anolisa_noop_accepts_omitted_version_fields() {
    let no_versions = br#"{"ok":true,"data":{"updated":false,"plan":[]}}"#;
    assert_eq!(
        parse_anolisa_output(no_versions, "cosh-ng", "1.2.3", AnolisaScope::User),
        Some(UpgradeVerdict::UpToDate)
    );
}

#[test]
fn anolisa_target_not_newer_than_running_version_is_up_to_date() {
    for latest in ["0.26.0", "0.25.0"] {
        let json = format!(
            r#"{{"ok":true,"data":{{"from_version":"0.24.1","to_version":"{latest}","plan":["replace"]}}}}"#
        );
        assert_eq!(
            parse_anolisa_output(json.as_bytes(), "cosh-ng", "0.26.0", AnolisaScope::User),
            Some(UpgradeVerdict::UpToDate)
        );
    }
}

#[test]
fn anolisa_preview_without_comparable_target_is_unavailable() {
    for json in [
        br#"{"ok":true,"data":{"from_version":"1.2.3","plan":["delegate"]}}"#.as_slice(),
        br#"{"ok":true,"data":{"to_version":"","plan":["replace"]}}"#.as_slice(),
        br#"{"ok":true,"data":{"to_version":"latest","plan":["replace"]}}"#.as_slice(),
    ] {
        assert_eq!(
            parse_anolisa_output(json, "cosh-ng", "1.2.3", AnolisaScope::User),
            Some(UpgradeVerdict::Unavailable)
        );
    }
}

#[test]
fn invalid_anolisa_payloads_fail_quietly() {
    assert_eq!(
        parse_anolisa_output(b"not-json", "cosh-ng", "1.2.3", AnolisaScope::User),
        None
    );
    assert_eq!(
        parse_anolisa_output(
            br#"{"ok":false,"data":{"plan":[]}}"#,
            "cosh-ng",
            "1.2.3",
            AnolisaScope::User,
        ),
        None
    );
}

#[test]
fn package_manager_exit_codes_and_candidate_are_classified() {
    assert_eq!(
        classify_package_manager_output(Some(0), b"", "cosh-ng", "1.0.0-1.el9", "dnf", None),
        Some(UpgradeVerdict::UpToDate)
    );
    let output = b"cosh-ng.x86_64 1.2.0-1.el9 updates\n";
    let result =
        classify_package_manager_output(Some(100), output, "cosh-ng", "1.0.0-1.el9", "dnf", None);
    assert!(matches!(
        result,
        Some(UpgradeVerdict::Upgrade(UpgradeNotice { current, command, .. }))
            if current == "1.0.0-1.el9" && command == "sudo dnf update cosh-ng"
    ));
    let delegated = classify_package_manager_output(
        Some(100),
        output,
        "cosh-ng",
        "1.0.0-1.el9",
        "dnf",
        Some("anolisa update cosh-ng"),
    );
    assert!(matches!(
        delegated,
        Some(UpgradeVerdict::Upgrade(UpgradeNotice { command, .. }))
            if command == "anolisa update cosh-ng"
    ));
    assert_eq!(
        classify_package_manager_output(Some(1), b"", "cosh-ng", "1.0.0-1.el9", "dnf", None),
        None
    );
}

#[test]
fn rpm_owner_preserves_the_actual_package_name_and_evr() {
    let installation =
        parse_rpm_installation(b"copilot-shell\n2:1.2.3-4.el9\n").expect("RPM installation");
    assert_eq!(installation.package, "copilot-shell");
    assert_eq!(installation.evr, "2:1.2.3-4.el9");
    assert!(parse_rpm_installation(b"copilot-shell\n").is_none());
}

#[test]
fn rpm_owner_omits_absent_and_zero_epochs_like_dnf() {
    for raw in [
        b"cosh-ng\n(none):0.24.1-1.alnx4\n".as_slice(),
        b"cosh-ng\n0:0.24.1-1.alnx4\n".as_slice(),
    ] {
        let installation = parse_rpm_installation(raw).expect("RPM installation");
        assert_eq!(installation.evr, "0.24.1-1.alnx4");
    }
}

#[test]
fn rpm_cached_notice_requires_the_same_installed_evr() {
    let cache = UpgradeCache {
        version: CACHE_VERSION,
        checked_at: 0,
        executable: PathBuf::from("/usr/libexec/anolisa/cosh-ng/cosh-shell"),
        method: DetectionMethod::Rpm,
        notice: Some(UpgradeNotice {
            package: "copilot-shell".to_string(),
            current: "0.26.0-1.el9".to_string(),
            latest: "0.26.0-2.el9".to_string(),
            command: "sudo dnf update copilot-shell".to_string(),
        }),
    };
    let matching = RpmInstallation {
        package: "copilot-shell".to_string(),
        evr: "0.26.0-1.el9".to_string(),
    };
    let cached = cached_notice(&cache, "0.26.0", Some(&matching)).expect("cached RPM notice");
    assert_eq!(cached.current, "0.26.0-1.el9");
    assert_eq!(cached.latest, "0.26.0-2.el9");

    let changed = RpmInstallation {
        evr: "0.26.0-2.el9".to_string(),
        ..matching
    };
    assert!(cached_notice(&cache, "0.26.0", Some(&changed)).is_none());
}

#[test]
fn anolisa_status_must_confirm_active_executable_ownership() {
    let executable = Path::new("/tmp/cosh-shell");
    let matching = br#"{"ok":true,"data":{"components":[{"active":true,"scope":"user","health":[{"name":"integrity:/tmp/cosh-shell","status":"ok"}]}]}}"#;
    assert_eq!(
        anolisa_status_owner_scope(matching, executable),
        Some(AnolisaScope::User)
    );

    let inactive = br#"{"ok":true,"data":{"components":[{"active":false,"scope":"user","health":[{"name":"integrity:/tmp/cosh-shell","status":"ok"}]}]}}"#;
    assert_eq!(anolisa_status_owner_scope(inactive, executable), None);

    let modified = br#"{"ok":true,"data":{"components":[{"active":true,"scope":"user","health":[{"name":"integrity:/tmp/cosh-shell","status":"sha256_mismatch"}]}]}}"#;
    assert_eq!(
        anolisa_status_owner_scope(modified, executable),
        Some(AnolisaScope::User)
    );
}

#[test]
fn anolisa_status_reports_the_scope_of_the_owning_record() {
    let executable = Path::new("/usr/local/libexec/anolisa/cosh-ng/cosh-shell");
    let status = br#"{"ok":true,"data":{"components":[
        {"active":false,"scope":"none","status":"not_installed"},
        {"active":true,"scope":"user","health":[{"name":"integrity:/home/u/.local/lib/anolisa/libexec/cosh-ng/cosh-shell"}]},
        {"active":true,"scope":"system","health":[{"name":"integrity:/usr/local/libexec/anolisa/cosh-ng/cosh-shell"}]}
    ]}}"#;
    assert_eq!(
        anolisa_status_owner_scope(status, executable),
        Some(AnolisaScope::System)
    );

    let unknown_scope = br#"{"ok":true,"data":{"components":[{"active":true,"scope":"none","health":[{"name":"integrity:/usr/local/libexec/anolisa/cosh-ng/cosh-shell"}]}]}}"#;
    assert_eq!(anolisa_status_owner_scope(unknown_scope, executable), None);
}

#[test]
fn version_compare_is_semver_aware_and_accepts_v_prefix() {
    assert_eq!(compare_versions("1.2.4", "1.2.3"), Some(Ordering::Greater));
    assert_eq!(compare_versions("v1.2.3", "1.2.3"), Some(Ordering::Equal));
    assert_eq!(
        compare_versions("1.2.3-rc.2", "1.2.3-rc.1"),
        Some(Ordering::Greater)
    );
    assert_eq!(compare_versions("1.2", "1.2.0"), None);
    assert_eq!(compare_versions("latest", "1.2.3"), None);
}

#[test]
fn cache_round_trip_suppresses_stale_and_corrupt_notices() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("cache directory");
    let path = directory.path().join("upgrade-check.json");
    let previous = env::var_os("COSH_SHELL_UPGRADE_CHECK_CACHE");
    env::set_var("COSH_SHELL_UPGRADE_CHECK_CACHE", &path);

    let executable = current_executable().expect("current executable");
    let result = DetectionResult {
        executable: executable.clone(),
        method: DetectionMethod::Anolisa,
        verdict: UpgradeVerdict::Upgrade(UpgradeNotice {
            package: "cosh-ng".to_string(),
            current: "0.24.1".to_string(),
            latest: "0.27.0".to_string(),
            command: "anolisa update cosh-ng".to_string(),
        }),
    };
    write_cache(&result).expect("write cache");
    let cached = read_cached_notice("0.26.0").expect("fresh cached notice");
    assert_eq!(cached.current, "0.26.0");
    assert_eq!(cached.latest, "0.27.0");
    assert!(read_cached_notice("0.27.0").is_none());

    let mismatched = DetectionResult {
        executable: executable.with_file_name("other-cosh-shell"),
        method: DetectionMethod::Anolisa,
        verdict: result.verdict.clone(),
    };
    write_cache(&mismatched).expect("write mismatched cache");
    assert!(read_cached_notice("0.26.0").is_none());

    let unsupported_version = serde_json::json!({
        "version": 0,
        "checked_at": 0,
        "executable": executable,
        "method": "anolisa",
        "notice": {
            "package": "cosh-ng",
            "current": "0.24.1",
            "latest": "0.27.0",
            "command": "anolisa update cosh-ng"
        }
    });
    fs::write(
        &path,
        serde_json::to_vec(&unsupported_version).expect("serialize unsupported cache"),
    )
    .expect("write unsupported cache");
    assert!(read_cached_notice("0.26.0").is_none());

    fs::write(&path, b"not-json").expect("corrupt cache");
    assert!(read_cached_notice("1.0.0").is_none());
    restore_env("COSH_SHELL_UPGRADE_CHECK_CACHE", previous);
}

#[test]
fn explicit_upgrade_check_opt_out_values_disable_probe() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let previous = env::var_os("COSH_SHELL_UPGRADE_CHECK");
    for value in ["off", "0", "false", "NO"] {
        env::set_var("COSH_SHELL_UPGRADE_CHECK", value);
        assert!(!startup_upgrade_check_enabled_for_env(), "{value}");
    }
    env::set_var("COSH_SHELL_UPGRADE_CHECK", "yes");
    assert!(startup_upgrade_check_enabled_for_env());
    restore_env("COSH_SHELL_UPGRADE_CHECK", previous);
}

#[cfg(unix)]
#[test]
fn fifo_cache_path_fails_quiet_without_blocking_the_login_thread() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;

    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("fifo cache directory");
    let path = directory.path().join("upgrade-check.fifo");
    mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).expect("create fifo");
    let previous = env::var_os("COSH_SHELL_UPGRADE_CHECK_CACHE");
    env::set_var("COSH_SHELL_UPGRADE_CHECK_CACHE", &path);

    // Run the read on a worker thread so a regressed metadata guard that falls
    // through to `fs::read` on the FIFO shows up as a timeout here instead of
    // hanging the whole test runner.
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let notice = read_cached_notice("0.26.0");
        let _ = sender.send(notice);
    });
    let notice = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("read_cached_notice must return without waiting for a FIFO writer");
    assert!(notice.is_none());
    worker.join().expect("join read_cached_notice worker");

    restore_env("COSH_SHELL_UPGRADE_CHECK_CACHE", previous);
}

#[test]
fn startup_state_resolves_probe_once() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let mut state = StartupUpgradeState::default();
    state.pending = Some(receiver);
    sender.send(None).expect("send verdict");
    state.poll_ready();
    assert_eq!(state.resolved, Some(None));
    assert!(state.pending.is_none());
}

fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
    if let Some(value) = value {
        env::set_var(name, value);
    } else {
        env::remove_var(name);
    }
}
