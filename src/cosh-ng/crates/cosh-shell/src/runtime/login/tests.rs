use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

use super::*;

fn account(shell: &str) -> Account {
    Account {
        name: "alice".into(),
        uid: 1001,
        gid: 1001,
        home: "/home/alice".into(),
        shell: shell.into(),
    }
}
fn record(phase: Phase) -> Record {
    Record {
        schema: 1,
        username: "alice".into(),
        uid: 1001,
        original_shell: "/bin/bash".into(),
        cosh_shell: COSH_PATH.into(),
        original_sha256: "a".repeat(64),
        phase,
    }
}
fn fixture() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::fixture(directory.path());
    (directory, store)
}
fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn login_parser_accepts_explicit_target_and_binds_privileged_confirmation() {
    let parsed = cli::parse(&os(&[
        "set",
        "--user",
        "alice",
        "--yes",
        "--__expected-uid",
        "1001",
    ]))
    .unwrap();
    assert_eq!(parsed.action, Action::Set);
    assert_eq!(parsed.user.as_deref(), Some("alice"));
    assert_eq!(parsed.expected_uid, Some(1001));
    assert!(parsed.yes);
    let default = cli::parse(&os(&["status"])).unwrap();
    assert!(default.user.is_none());
    for args in [
        vec![],
        vec!["invalid"],
        vec!["status", "--yes"],
        vec!["set", "--user"],
        vec!["set", "--user", "-root"],
        vec!["set", "--user", "a/b"],
        vec!["set", "--yes", "--yes"],
        vec!["restore", "--__expected-uid", "0"],
        vec!["status", "--user", "alice", "--user", "root"],
    ] {
        assert!(cli::parse(&os(&args)).is_err(), "{args:?}");
    }
}

#[test]
fn nss_must_match_one_local_account_and_never_accept_directory_or_uid_aliases() {
    let local = "root:x:0:0:root:/root:/bin/bash\nalice:x:1001:1001:Alice:/home/alice:/bin/bash\n";
    assert_eq!(
        account::match_local(account("/bin/bash"), local)
            .unwrap()
            .name,
        "alice"
    );
    for bad in [
        local.replace("/bin/bash", "/bin/zsh"),
        local.replace("1001:1001", "1002:1001"),
        format!("{local}other:x:1001:1001::/home/other:/bin/bash\n"),
        "root:x:0:0::/root:/bin/bash\n".into(),
        format!("{local}malformed\n"),
    ] {
        assert!(
            account::match_local(account("/bin/bash"), &bad).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn set_restore_is_idempotent_and_does_not_freeze_or_replace_the_original_bash_hash() {
    let (_directory, store) = fixture();
    let current = RefCell::new(account("/bin/bash"));
    let expected = current.borrow().clone();
    let writes = Cell::new(0);
    for (action, hash) in [
        (Action::Set, "a".repeat(64)),
        (Action::Set, "b".repeat(64)),
        (Action::Restore, "c".repeat(64)),
        (Action::Restore, "d".repeat(64)),
    ] {
        let result = transact(
            action,
            &expected,
            &hash,
            &store,
            || Ok(()),
            || Ok(current.borrow().clone()),
            |target, _| {
                assert_eq!(store.read(1001).unwrap().unwrap().phase, Phase::Prepared);
                writes.set(writes.get() + 1);
                current.borrow_mut().shell = target.into();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            result.account.shell,
            if action == Action::Set {
                COSH_PATH
            } else {
                "/bin/bash"
            }
        );
        assert_eq!(
            store.read(1001).unwrap().unwrap().original_sha256,
            "a".repeat(64)
        );
    }
    assert_eq!(writes.get(), 2);
    assert_eq!(store.read(1001).unwrap().unwrap().phase, Phase::Restored);
}

#[test]
fn prepared_record_recovers_from_actual_cosh_without_reapplying_the_account_tool() {
    let (_directory, store) = fixture();
    store.write(&record(Phase::Prepared)).unwrap();
    let current = account(COSH_PATH);
    let recovered = transact(
        Action::Set,
        &current,
        &"b".repeat(64),
        &store,
        || Ok(()),
        || Ok(current.clone()),
        |_, _| panic!("already changed account must not be changed again"),
    )
    .unwrap();
    assert_eq!(recovered.phase, Some(Phase::Applied));
    assert_eq!(
        store.read(1001).unwrap().unwrap().original_shell,
        "/bin/bash"
    );
}

#[test]
fn external_third_shell_and_untrusted_or_missing_records_never_restore() {
    let (directory, store) = fixture();
    assert!(transact(
        Action::Restore,
        &account(COSH_PATH),
        "",
        &store,
        || Ok(()),
        || Ok(account(COSH_PATH)),
        |_, _| panic!("missing record cannot restore")
    )
    .is_err());
    store.write(&record(Phase::Applied)).unwrap();
    assert!(transact(
        Action::Restore,
        &account("/bin/zsh"),
        "",
        &store,
        || Ok(()),
        || Ok(account("/bin/zsh")),
        |_, _| panic!("third shell must not be replaced")
    )
    .is_err());
    assert_eq!(store.read(1001).unwrap().unwrap().phase, Phase::Conflict);
    let mut wrong_uid = record(Phase::Applied);
    wrong_uid.uid = 1002;
    fs::write(
        directory.path().join("1001.json"),
        serde_json::to_vec(&wrong_uid).unwrap(),
    )
    .unwrap();
    assert!(status(account(COSH_PATH), store.read(1001).unwrap().as_ref()).is_err());
    fs::write(directory.path().join("1001.json"), b"{broken").unwrap();
    assert!(store.read(1001).is_err());
}

#[test]
fn write_time_uid_or_shell_changes_stop_before_the_account_tool() {
    for uid_change in [false, true] {
        let (_directory, store) = fixture();
        let reads = Cell::new(0);
        let result = transact(
            Action::Set,
            &account("/bin/bash"),
            &"a".repeat(64),
            &store,
            || Ok(()),
            || {
                reads.set(reads.get() + 1);
                let mut current = account("/bin/bash");
                if reads.get() > 1 {
                    if uid_change {
                        current.uid = 1002;
                    } else {
                        current.shell = "/bin/zsh".into();
                    }
                }
                Ok(current)
            },
            |_, _| panic!("stale identity must not reach system account tool"),
        );
        assert!(result.is_err());
        assert_eq!(store.read(1001).unwrap().unwrap().phase, Phase::Conflict);
    }
}

#[test]
fn failed_post_write_verification_keeps_a_conflict_record() {
    let (_directory, store) = fixture();
    assert!(transact(
        Action::Set,
        &account("/bin/bash"),
        &"a".repeat(64),
        &store,
        || Ok(()),
        || Ok(account("/bin/bash")),
        |_, _| Ok(())
    )
    .is_err());
    assert_eq!(store.read(1001).unwrap().unwrap().phase, Phase::Conflict);
}

#[test]
fn administrator_record_store_rejects_links_permissions_corruption_and_lock_contention() {
    let (directory, store) = fixture();
    let target = directory.path().join("1001.json");
    let outside = directory.path().join("outside");
    fs::write(&outside, b"{}").unwrap();
    symlink(&outside, &target).unwrap();
    assert!(store.read(1001).is_err());
    assert!(store.write(&record(Phase::Prepared)).is_err());
    fs::remove_file(&target).unwrap();
    fs::hard_link(&outside, &target).unwrap();
    assert!(store.read(1001).is_err());
    fs::remove_file(&target).unwrap();
    store.write(&record(Phase::Prepared)).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(store.read(1001).is_err());
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let lock = store.lock().unwrap();
    assert!(store.lock().is_err());
    drop(lock);
    assert!(store.lock().is_ok());
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(store.read(1001).is_err());
}

#[test]
fn release_capability_requires_the_explicit_supported_package_lifecycle_contract() {
    let value = serde_json::json!({"schema":1,"login_record_schema":1,"stable_entry":COSH_PATH,
        "package":"cosh-ng","minimum_compatible_package_version":"0.27.0",
        "replacement_preflight_protocol":1,"replacement_preflight":"/usr/libexec/cosh/login-preflight",
        "final_uninstall_guard":true});
    let proof: capability::Proof = serde_json::from_value(value.clone()).unwrap();
    assert!(proof.validate("0.27.0").is_ok());
    assert!(proof.validate("0.26.0").is_err());
    for (key, bad) in [
        ("schema", serde_json::json!(2)),
        ("stable_entry", serde_json::json!("/tmp/cosh")),
        ("replacement_preflight", serde_json::json!("/tmp/helper")),
        ("final_uninstall_guard", serde_json::json!(false)),
    ] {
        let mut invalid = value.clone();
        invalid[key] = bad;
        let proof: capability::Proof = serde_json::from_value(invalid).unwrap();
        assert!(proof.validate("0.27.0").is_err());
    }
    let mut missing = value;
    missing
        .as_object_mut()
        .unwrap()
        .remove("replacement_preflight");
    assert!(serde_json::from_value::<capability::Proof>(missing).is_err());
}

#[test]
fn login_management_refuses_platforms_outside_the_validated_alinux4_rpm_scope() {
    assert!(account::validate_platform("ID=alinux\nVERSION_ID=\"4\"\n").is_ok());
    for release in [
        "ID=ubuntu\nVERSION_ID=24.04",
        "ID=alinux\nVERSION_ID=3",
        "ID=alinux\nVERSION_ID=4\nID=ubuntu",
    ] {
        assert!(account::validate_platform(release).is_err());
    }
    assert!(account::supported_bash("/bin/bash"));
    assert!(account::supported_bash("/usr/bin/bash"));
    for path in [
        "bash",
        "/home/alice/bash",
        "/usr/local/bin/bash",
        "/bin/zsh",
        "/sbin/nologin",
    ] {
        assert!(!account::supported_bash(path));
    }
}

#[test]
fn recovery_record_rejects_wrong_name_schema_and_non_system_shell_paths() {
    for mutation in 0..4 {
        let mut record = record(Phase::Applied);
        match mutation {
            0 => record.username = "root".into(),
            1 => record.schema = 2,
            2 => record.original_shell = "/home/alice/bash".into(),
            _ => record.cosh_shell = "/tmp/cosh".into(),
        }
        assert!(record.validate(&account(COSH_PATH)).is_err());
    }
}

#[test]
fn read_only_status_projection_exposes_only_valid_managed_recovery_paths() {
    let managed =
        LoginStatus::from(status(account(COSH_PATH), Some(&record(Phase::Applied))).unwrap());
    assert!(managed.managed);
    assert_eq!(managed.original_bash.as_deref(), Some("/bin/bash"));
    let external =
        LoginStatus::from(status(account("/bin/zsh"), Some(&record(Phase::Applied))).unwrap());
    assert!(!external.managed);
    assert!(external.conflict);
    assert_eq!(external.transaction, "conflict");
    let missing = LoginStatus::from(status(account(COSH_PATH), None).unwrap());
    assert!(!missing.managed);
    assert!(!missing.conflict);
    assert!(missing.original_bash.is_none());
}

#[test]
fn existing_default_cosh_without_a_record_is_unmanaged_and_never_guesses_a_restore() {
    let snapshot = status(account(COSH_PATH), None).unwrap();
    assert!(
        !snapshot.conflict,
        "legacy default Cosh is not a corrupt managed account"
    );
    assert!(snapshot.original_shell.is_none());
    let (_directory, store) = fixture();
    assert!(transact(
        Action::Restore,
        &account(COSH_PATH),
        "",
        &store,
        || Ok(()),
        || Ok(account(COSH_PATH)),
        |_, _| panic!("unrecorded login must not guess a restore")
    )
    .is_err());
}

#[test]
fn managed_login_prefers_its_record_and_legacy_or_bad_records_use_only_verified_system_bash() {
    let mut managed = record(Phase::Applied);
    managed.original_shell = "/usr/bin/bash".into();
    let plan = launch::select_login_bash(&account(COSH_PATH), Ok(Some(managed)), |path| {
        assert_eq!(path, "/usr/bin/bash");
        Ok(())
    })
    .unwrap();
    assert_eq!(plan.bash_path, "/usr/bin/bash");
    assert!(plan.warning.is_none());
    let mut entry = crate::runtime::invocation::TuiEntry {
        shell_kind: Some(crate::runtime::cli_args::RawShellKind::Zsh),
        ..Default::default()
    };
    launch::apply_login_shell_plan(&mut entry, plan);
    let (_, shell, options) = crate::runtime::cli_args::configured_raw_invocation(
        entry,
        &crate::config::CoshConfig::default(),
    );
    assert_eq!(shell, crate::runtime::cli_args::RawShellKind::Bash);
    assert_eq!(options.trusted_login_bash.as_deref(), Some("/usr/bin/bash"));
    for bad in [false, true] {
        let record = if bad {
            Err(Error::new("corrupt record"))
        } else {
            Ok(None)
        };
        let plan = launch::select_login_bash(&account(COSH_PATH), record, |path| {
            if path == "/bin/bash" {
                Err(Error::new("unavailable"))
            } else if path == "/usr/bin/bash" {
                Ok(())
            } else {
                panic!("not a fixed system path")
            }
        })
        .unwrap();
        assert_eq!(plan.bash_path, "/usr/bin/bash");
        assert_eq!(plan.warning.is_some(), bad);
    }
    let mut untrusted = super::tests::record(Phase::Applied);
    untrusted.original_shell = "/home/alice/evil-bash".into();
    let plan = launch::select_login_bash(&account(COSH_PATH), Ok(Some(untrusted)), |path| {
        assert!(account::supported_bash(path));
        Ok(())
    })
    .unwrap();
    assert_eq!(plan.bash_path, "/bin/bash");
    assert!(plan.warning.is_some());
    assert!(
        launch::select_login_bash(&account(COSH_PATH), Ok(None), |_| Err(Error::new(
            "no safe Bash"
        )))
        .is_err()
    );
}

#[test]
fn restore_recovers_trusted_bash_when_installed_cosh_is_broken() {
    let (_directory, store) = fixture();
    store.write(&record(Phase::Applied)).unwrap();
    let current = RefCell::new(account(COSH_PATH));
    let expected = current.borrow().clone();
    let hash = account::verify_mutation_with(
        Action::Restore,
        "/bin/bash",
        |_| Ok("b".repeat(64)),
        || Err(Error::new("Cosh entry is missing or damaged")),
        || Ok("/bin/bash\n".into()),
    )
    .expect("restore only requires its trusted destination");
    let result = transact(
        Action::Restore,
        &expected,
        &hash,
        &store,
        || Ok(()),
        || Ok(current.borrow().clone()),
        |target, _| {
            current.borrow_mut().shell = target.into();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(result.account.shell, "/bin/bash");
    assert_eq!(result.phase, Some(Phase::Restored));
    assert!(account::verify_mutation_with(
        Action::Set,
        "/bin/bash",
        |_| Ok("b".repeat(64)),
        || Err(Error::new("damaged Cosh")),
        || Ok("/bin/bash\n".into())
    )
    .is_err());
}

#[test]
fn managed_shell_assets_must_be_readable_and_executable_by_the_target_account() {
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("shell");
    fs::write(&program, b"\x7fELFfixture").unwrap();
    for mode in [0o700, 0o750, 0o751, 0o711, 0o555] {
        fs::set_permissions(&program, fs::Permissions::from_mode(mode)).unwrap();
        let metadata = fs::metadata(&program).unwrap();
        let accepted =
            account::validate_public_execution(metadata.permissions().mode(), false, false).is_ok();
        assert_eq!(
            accepted,
            mode == 0o555,
            "mode {mode:o} must not be judged by root access"
        );
    }
    assert!(
        account::validate_public_execution(0o755, false, true).is_err(),
        "extended ACL permissions are unsupported"
    );
    assert!(
        account::validate_public_execution(0o700, true, false).is_err(),
        "target cannot traverse root-only parent"
    );
    assert!(account::validate_public_execution(0o711, true, false).is_ok());
}

#[test]
fn slow_validation_finishes_before_the_last_account_identity_sample() {
    let (_directory, store) = fixture();
    let current = RefCell::new(account("/bin/bash"));
    let writes = Cell::new(0);
    let result = transact(
        Action::Set,
        &account("/bin/bash"),
        &"a".repeat(64),
        &store,
        || {
            current.borrow_mut().shell = "/bin/zsh".into();
            Ok(())
        },
        || Ok(current.borrow().clone()),
        |target, _| {
            writes.set(writes.get() + 1);
            current.borrow_mut().shell = target.into();
            Ok(())
        },
    );
    assert!(
        result.is_err(),
        "an administrator's shell change during slow validation must stop before the account tool"
    );
    assert_eq!(writes.get(), 0);
    assert_eq!(current.borrow().shell, "/bin/zsh");
}

#[test]
fn alinux_account_tools_match_the_verified_shadow_build_and_reject_unknown_assets() {
    for arch in ["aarch64", "x86_64"] {
        let package = format!("shadow 4.14.3-2.alnx4 {arch}\n");
        for tool in ["/usr/sbin/usermod", "/usr/sbin/vipw"] {
            assert!(account::account_tool_package_matches(
                tool,
                package.as_bytes(),
                arch
            ));
            for unknown in [
                format!("shadow-utils 4.14.3-2.alnx4 {arch}\n"),
                format!("shadow 4.15.0-1.alnx4 {arch}\n"),
                format!("shadow 4.14.3-3.alnx4 {arch}\n"),
                format!("shadow 4.14.3-2.alnx4 {arch}"),
                package.repeat(2),
            ] {
                assert!(!account::account_tool_package_matches(
                    tool,
                    unknown.as_bytes(),
                    arch
                ));
            }
        }
        assert!(!account::account_tool_package_matches(
            "/usr/local/sbin/vipw",
            package.as_bytes(),
            arch
        ));
        let other = if arch == "aarch64" {
            "x86_64"
        } else {
            "aarch64"
        };
        assert!(!account::account_tool_package_matches(
            "/usr/sbin/vipw",
            package.as_bytes(),
            other
        ));
    }
    assert!(!account::account_tool_package_matches(
        "/usr/sbin/vipw",
        b"shadow 4.14.3-2.alnx4 riscv64\n",
        "riscv64"
    ));
}

#[test]
fn native_account_failure_preserves_the_previous_phase_only_when_the_account_is_exactly_unchanged()
{
    for third_shell in [false, true] {
        let (_directory, store) = fixture();
        store.write(&record(Phase::Applied)).unwrap();
        let current = RefCell::new(account(COSH_PATH));
        let expected = current.borrow().clone();
        assert!(transact(
            Action::Restore,
            &expected,
            "",
            &store,
            || Ok(()),
            || Ok(current.borrow().clone()),
            |_, _| {
                if third_shell {
                    current.borrow_mut().shell = "/bin/zsh".into();
                }
                Err(Error::new("native account transaction refused"))
            }
        )
        .is_err());
        assert_eq!(
            store.read(1001).unwrap().unwrap().phase,
            if third_shell {
                Phase::Conflict
            } else {
                Phase::Applied
            }
        );
    }
}

#[test]
fn native_account_failure_with_an_unreadable_post_state_is_conflict() {
    let (_directory, store) = fixture();
    store.write(&record(Phase::Applied)).unwrap();
    let reads = Cell::new(0);
    assert!(transact(
        Action::Restore,
        &account(COSH_PATH),
        "",
        &store,
        || Ok(()),
        || {
            reads.set(reads.get() + 1);
            if reads.get() >= 3 {
                Err(Error::new("NSS unavailable after account operation"))
            } else {
                Ok(account(COSH_PATH))
            }
        },
        |_, _| Err(Error::new("native transaction refused"))
    )
    .is_err());
    assert_eq!(store.read(1001).unwrap().unwrap().phase, Phase::Conflict);
}

#[test]
fn lifecycle_record_scan_keeps_restored_records_and_rejects_unknown_entries() {
    let (directory, store) = fixture();
    let active = record(Phase::Applied);
    store.write(&active).unwrap();
    let mut restored = record(Phase::Restored);
    restored.uid = 1002;
    restored.username = "bob".into();
    store.write(&restored).unwrap();
    let mut records = store.records().unwrap();
    records.sort_by_key(|record| record.uid);
    assert_eq!(records, vec![active, restored]);
    fs::write(directory.path().join("unknown.json"), b"{}").unwrap();
    assert!(store.records().is_err());
}

#[test]
fn lifecycle_record_scan_validates_private_lock_metadata_without_reading_contents() {
    let (directory, store) = fixture();
    store.write(&record(Phase::Applied)).unwrap();
    drop(store.lock().unwrap());
    fs::set_permissions(
        directory.path().join(".lock"),
        fs::Permissions::from_mode(0o0),
    )
    .unwrap();
    assert_eq!(store.records().unwrap().len(), 1);
}
