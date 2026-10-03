use super::*;

#[test]
fn anolisa_preview_compares_target_with_running_version() {
    let json = br#"{"ok":true,"data":{"package":"cosh-ng","to_version":"1.3.0","updated":false,"plan":["replace"]}}"#;
    assert_eq!(
        parse_anolisa_output(json, "cosh-ng", "1.2.3"),
        Some(UpgradeVerdict::Upgrade(UpgradeNotice {
            package: "cosh-ng".to_string(),
            current: "1.2.3".to_string(),
            latest: "1.3.0".to_string(),
            command: "anolisa update cosh-ng".to_string(),
        }))
    );
}

#[test]
fn anolisa_noop_accepts_omitted_version_fields() {
    let no_versions = br#"{"ok":true,"data":{"updated":false,"plan":[]}}"#;
    assert_eq!(
        parse_anolisa_output(no_versions, "cosh-ng", "1.2.3"),
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
            parse_anolisa_output(json.as_bytes(), "cosh-ng", "0.26.0"),
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
            parse_anolisa_output(json, "cosh-ng", "1.2.3"),
            Some(UpgradeVerdict::Unavailable)
        );
    }
}

#[test]
fn invalid_anolisa_payloads_fail_quietly() {
    assert_eq!(parse_anolisa_output(b"not-json", "cosh-ng", "1.2.3"), None);
    assert_eq!(
        parse_anolisa_output(br#"{"ok":false,"data":{"plan":[]}}"#, "cosh-ng", "1.2.3",),
        None
    );
}

#[test]
fn package_manager_exit_codes_and_candidate_are_classified() {
    assert_eq!(
        classify_package_manager_output(Some(0), b"", "cosh-ng", "1.0.0", "dnf", None),
        Some(UpgradeVerdict::UpToDate)
    );
    let output = b"cosh-ng.x86_64 1.2.0-1.el9 updates\n";
    let result =
        classify_package_manager_output(Some(100), output, "cosh-ng", "1.0.0", "dnf", None);
    assert!(matches!(
        result,
        Some(UpgradeVerdict::Upgrade(UpgradeNotice { command, .. }))
            if command == "sudo dnf update cosh-ng"
    ));
    let delegated = classify_package_manager_output(
        Some(100),
        output,
        "cosh-ng",
        "1.0.0",
        "dnf",
        Some("anolisa update cosh-ng"),
    );
    assert!(matches!(
        delegated,
        Some(UpgradeVerdict::Upgrade(UpgradeNotice { command, .. }))
            if command == "anolisa update cosh-ng"
    ));
    assert_eq!(
        classify_package_manager_output(Some(1), b"", "cosh-ng", "1.0.0", "dnf", None),
        None
    );
}

#[test]
fn rpm_ownership_accepts_only_the_cosh_ng_package() {
    assert_eq!(
        parse_rpm_package_name(b"cosh-ng"),
        Some("cosh-ng".to_string())
    );
    assert_eq!(
        parse_rpm_package_name(b"cosh-ng\n"),
        Some("cosh-ng".to_string())
    );
    assert_eq!(
        parse_rpm_package_name(b"  cosh-ng  \n"),
        Some("cosh-ng".to_string())
    );
    assert_eq!(parse_rpm_package_name(b"copilot-shell"), None);
    // Arch-qualified or sibling package names must never be accepted; only an
    // exact `cosh-ng` NAME enables the RPM fallback.
    assert_eq!(parse_rpm_package_name(b"cosh-ng.x86_64"), None);
    assert_eq!(parse_rpm_package_name(b"cosh-ng-extras"), None);
    assert_eq!(parse_rpm_package_name(b""), None);
}

#[test]
fn managed_anolisa_paths_resolve_the_matching_cli() {
    assert_eq!(
        anolisa_candidate_for_managed_executable(Path::new(
            "/usr/local/libexec/anolisa/cosh-ng/cosh-shell"
        )),
        Some(PathBuf::from("/usr/local/bin/anolisa"))
    );
    assert_eq!(
        anolisa_candidate_for_managed_executable(Path::new(
            "/home/test/.local/lib/anolisa/libexec/cosh-ng/cosh-shell"
        )),
        Some(PathBuf::from("/home/test/.local/bin/anolisa"))
    );
    assert_eq!(
        anolisa_candidate_for_managed_executable(Path::new(
            "/usr/libexec/anolisa/cosh-ng/cosh-shell"
        )),
        None
    );
    assert_eq!(
        anolisa_candidate_for_managed_executable(Path::new("/workspace/target/debug/cosh-shell")),
        None
    );
    // A managed leaf must be named `cosh-shell`; any other binary in the tree
    // is not the owned executable.
    assert_eq!(
        anolisa_candidate_for_managed_executable(Path::new(
            "/usr/local/libexec/anolisa/cosh-ng/other-binary"
        )),
        None
    );
    // The immediate parent directory must be `cosh-ng`.
    assert_eq!(
        anolisa_candidate_for_managed_executable(Path::new(
            "/usr/local/libexec/anolisa/wrong/cosh-shell"
        )),
        None
    );
    // The user layout requires the dotted `.local` root; a plain `local` is not
    // a managed install.
    assert_eq!(
        anolisa_candidate_for_managed_executable(Path::new(
            "/home/test/local/lib/anolisa/libexec/cosh-ng/cosh-shell"
        )),
        None
    );
    // A hand-copied binary outside any managed tree stays silent.
    assert_eq!(
        anolisa_candidate_for_managed_executable(Path::new("/home/user/bin/cosh-shell")),
        None
    );
}

#[test]
fn unmanaged_executable_never_borrows_configured_or_path_anolisa() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("anolisa bin directory");
    let anolisa = directory.path().join("anolisa");
    fs::write(&anolisa, b"#!/bin/sh\n").expect("write anolisa stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&anolisa, fs::Permissions::from_mode(0o755))
            .expect("mark anolisa executable");
    }
    let previous = env::var_os("COSH_SHELL_ANOLISA_BIN");
    env::set_var("COSH_SHELL_ANOLISA_BIN", &anolisa);

    // A locally built or hand-copied binary matches no managed layout, so the
    // managed lookup stays silent even when a configured anolisa is present and
    // executable. This guarantees an unmanaged executable never borrows an
    // unrelated anolisa (configured or on PATH).
    assert_eq!(
        locate_managed_anolisa(Path::new("/workspace/target/debug/cosh-shell")),
        None
    );
    assert_eq!(
        locate_managed_anolisa(Path::new("/home/user/bin/cosh-shell")),
        None
    );

    // A managed layout resolves, and the configured override wins over the
    // path derived from the executable location.
    assert_eq!(
        locate_managed_anolisa(Path::new("/usr/local/libexec/anolisa/cosh-ng/cosh-shell")),
        Some(anolisa.clone())
    );

    restore_env("COSH_SHELL_ANOLISA_BIN", previous);
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

    // A legacy version-1 cache entry is rejected even when the executable path
    // matches, so the schema bump (cache version 2 bound to the executable)
    // never resurrects a pre-migration notice.
    let legacy = serde_json::json!({
        "version": 1,
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
        serde_json::to_vec(&legacy).expect("serialize legacy cache"),
    )
    .expect("write legacy cache");
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

#[test]
fn startup_state_resolves_probe_once() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let mut state = StartupUpgradeState {
        pending: Some(receiver),
        ..StartupUpgradeState::default()
    };
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
