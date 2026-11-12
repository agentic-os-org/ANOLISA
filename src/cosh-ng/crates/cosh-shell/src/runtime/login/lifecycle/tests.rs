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

fn state_fixture() -> (tempfile::TempDir, Store) {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let store = Store::fixture(&root);
    (directory, store)
}

#[test]
fn restored_rollback_reupgrade_cycle_clears_completed_records_and_retains_lock_inode() {
    use crate::runtime::login::{transact, Action as LoginAction};
    use std::cell::RefCell;
    use std::os::unix::fs::MetadataExt;
    let (directory, store) = state_fixture();
    let current = RefCell::new(account("/bin/bash"));
    let expected = current.borrow().clone();
    for action in [LoginAction::Set, LoginAction::Restore] {
        transact(
            action,
            &expected,
            &"a".repeat(64),
            &store,
            || Ok(()),
            || Ok(current.borrow().clone()),
            |target, _| {
                current.borrow_mut().shell = target.into();
                Ok(())
            },
        )
        .unwrap();
    }
    let lease = store.lock().unwrap();
    let inode = lease.metadata().unwrap().ino();
    let completed = store.records().unwrap();
    assert_eq!(completed[0].phase, Phase::Restored);
    check_state(&completed, &[current.borrow().clone()], false).unwrap();
    store.clear_restored(&completed).unwrap();
    assert!(store.records().unwrap().is_empty());
    assert_eq!(
        std::fs::metadata(directory.path().join(".lock"))
            .unwrap()
            .ino(),
        inode
    );
    assert!(store.lock().is_err());
    drop(lease);
    transact(
        LoginAction::Set,
        &expected,
        &"b".repeat(64),
        &store,
        || Ok(()),
        || Ok(current.borrow().clone()),
        |target, _| {
            current.borrow_mut().shell = target.into();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        store.read(expected.uid).unwrap().unwrap().phase,
        Phase::Applied
    );
}

#[test]
fn controlled_unknown_transaction_cleans_completed_state_but_compatible_upgrade_keeps_it() {
    let (_directory, store) = state_fixture();
    let _lease = store.lock().unwrap();
    let restored = record(Phase::Restored);
    store.write(&restored).unwrap();
    let fixture_audit = |store: &Store, compatible| {
        let records = store.records()?;
        check_state(&records, &[account("/bin/bash")], compatible)?;
        Ok(records)
    };
    prepare_transaction(&store, true, fixture_audit).unwrap();
    assert_eq!(store.records().unwrap(), vec![restored]);
    assert!(require_erase_cleanup(&store.records().unwrap()).is_err());
    prepare_transaction(&store, false, fixture_audit).unwrap();
    assert!(store.records().unwrap().is_empty());
    require_erase_cleanup(&store.records().unwrap()).unwrap();
    for shell in [
        COSH_PATH,
        "/bin/cosh",
        "/usr/bin/cosh-login",
        "/bin/cosh-login",
    ] {
        let result = prepare_transaction(&store, false, |store, compatible| {
            let records = store.records()?;
            check_state(&records, &[account(shell)], compatible)?;
            Ok(records)
        });
        assert!(result.is_err(), "{shell}");
        assert!(store.records().unwrap().is_empty());
    }
}

#[test]
fn record_cleanup_never_deletes_active_changed_or_unknown_entries() {
    let (directory, store) = state_fixture();
    let _lease = store.lock().unwrap();
    for phase in [Phase::Prepared, Phase::Applied, Phase::Conflict] {
        let active = record(phase);
        store.write(&active).unwrap();
        assert!(store.clear_restored(std::slice::from_ref(&active)).is_err());
        assert_eq!(store.read(active.uid).unwrap(), Some(active));
    }
    let restored = record(Phase::Restored);
    store.write(&restored).unwrap();
    let mut changed = restored.clone();
    changed.original_sha256 = "b".repeat(64);
    store.write(&changed).unwrap();
    assert!(store
        .clear_restored(std::slice::from_ref(&restored))
        .is_err());
    assert_eq!(store.read(changed.uid).unwrap(), Some(changed));
    store.write(&restored).unwrap();
    std::fs::write(
        directory
            .path()
            .join(".aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tmp"),
        b"unfinished",
    )
    .unwrap();
    assert!(store
        .clear_restored(std::slice::from_ref(&restored))
        .is_err());
    assert_eq!(store.read(restored.uid).unwrap(), Some(restored));
}

#[test]
fn lifecycle_arguments_are_exact_and_owned_once() {
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    assert_eq!(
        parse(&args(&[
            "--verify-installed",
            "--entry",
            COSH_PATH,
            "--record-version",
            "1"
        ]))
        .unwrap(),
        Action::VerifyInstalled
    );
    assert_eq!(
        parse(&args(&[
            "check-protocol",
            "1",
            "--record-version",
            "1",
            "--entry",
            COSH_PATH
        ]))
        .unwrap(),
        Action::CheckProtocol
    );
    assert_eq!(parse(&args(&["check-erase"])).unwrap(), Action::CheckErase);
    assert!(parse(&args(&["erase"])).is_ok());
    assert_eq!(
        parse(&args(&["replace", "/root/target.rpm"])).unwrap(),
        Action::Replace("/root/target.rpm".into())
    );
    assert_eq!(
        parse(&args(&["downgrade", "/root/old.rpm"])).unwrap(),
        Action::Downgrade("/root/old.rpm".into())
    );
    for invalid in [
        vec![],
        vec!["check-erase", "extra"],
        vec!["check-protocol", "2"],
        vec!["replace", "relative.rpm"],
        vec!["replace", "/root/../target.rpm"],
        vec!["replace", "/root/a.rpm", "--yes"],
        vec!["status"],
        vec!["swap-to-copilot"],
    ] {
        assert!(parse(&args(&invalid)).is_err());
    }
}

#[test]
fn target_capability_is_bound_to_fixed_rpm_header_protocol_and_version() {
    assert!(compatible_header(b"cosh-ng\n0.27.0\ncosh-login-lifecycle-v1\n").unwrap());
    assert!(!compatible_header(b"cosh-ng\n0.27.0\ncosh-ng\n").unwrap());
    assert!(!compatible_header(b"cosh-ng\n0.26.0\ncosh-login-lifecycle-v1\n").unwrap());
    assert!(!compatible_header(b"cosh-ng\nlegacy-version\ncosh-login-lifecycle-v1\n").unwrap());
    assert!(compatible_header(b"copilot-shell\n0.27.0\ncosh-login-lifecycle-v1\n").is_err());
    assert!(compatible_header(b"cosh-ng\n0.27.0\ncosh-login-lifecycle-v1\0\n").is_err());
    assert!(compatible_header(&vec![b'a'; 8193]).is_err());
}

#[test]
fn only_installed_capability_audit_is_available_before_sudo() {
    assert!(admit(&Action::VerifyInstalled, 1001).is_ok());
    for action in [
        Action::CheckErase,
        Action::Erase,
        Action::CheckProtocol,
        Action::CheckTarget("/root/no-access.rpm".into()),
        Action::Replace("/root/no-access.rpm".into()),
        Action::Downgrade("/root/no-access.rpm".into()),
    ] {
        assert!(admit(&action, 1001).is_err());
        assert!(admit(&action, 0).is_ok());
    }
}

#[test]
fn transaction_exec_preserves_only_the_exact_store_lock_descriptor() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let store = super::super::Store::fixture(&root);
    let lock = store.lock().unwrap();
    let unrelated = File::open("/dev/null").unwrap();
    let flags = |file: &File| unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETFD) };
    assert_ne!(flags(&lock) & nix::libc::FD_CLOEXEC, 0);
    inherit_lock(&lock).unwrap();
    assert_eq!(flags(&lock) & nix::libc::FD_CLOEXEC, 0);
    assert_ne!(flags(&unrelated) & nix::libc::FD_CLOEXEC, 0);
    assert!(store.lock().is_err());
}

#[test]
fn invalid_lifecycle_leaf_is_usage_error_without_state_access() {
    assert_eq!(run_cli(&[]), 2);
    assert_eq!(run_cli(&["check-erase".into(), "secret-extra".into()]), 2);
}

#[test]
fn rpm_assets_gate_capability_and_route_lifecycle_through_one_backend() {
    let spec = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packaging/rpm/cosh-ng.spec.in"
    ));
    assert!(spec.contains("%bcond_with login_lifecycle"));
    assert!(!spec.contains("/profile.d/"));
    assert!(!spec.contains("packaging/bash/cosh-profile.sh"));
    assert!(!spec.contains("packaging/system-integration"));
    assert!(!spec.contains("bashrc-source.bash"));
    assert!(spec.contains("packaging/bash/cosh-integration.bash"));
    assert!(spec.contains("%if %{with login_lifecycle}\nProvides:       cosh-login-lifecycle-v1"));
    assert!(spec.contains("check-protocol 1 --record-version 1 --entry /usr/bin/cosh"));
    assert!(spec.contains("/login-preflight check-erase"));
    assert!(!spec.contains("cosh_replacement_ready"));
    assert!(!spec.contains("sudo yum swap"));
}

#[test]
fn unknown_target_refuses_managed_records_before_any_package_transaction() {
    for phase in [Phase::Prepared, Phase::Applied, Phase::Conflict] {
        assert!(check_state(&[record(phase)], &[account(COSH_PATH)], false).is_err());
    }
}

#[test]
fn restored_records_only_allow_unknown_target_after_actual_reference_is_clear() {
    let restored = record(Phase::Restored);
    assert!(check_state(
        std::slice::from_ref(&restored),
        &[account("/bin/bash")],
        false
    )
    .is_ok());
    for shell in [
        COSH_PATH,
        "/bin/cosh",
        "/usr/bin/cosh-login",
        "/bin/cosh-login",
    ] {
        for compatible in [false, true] {
            assert!(
                check_state(
                    std::slice::from_ref(&restored),
                    &[account(shell)],
                    compatible
                )
                .is_err(),
                "{shell}"
            );
        }
    }
}

#[test]
fn unknown_target_and_final_erase_refuse_unrecorded_nss_references() {
    for shell in [
        COSH_PATH,
        "/bin/cosh",
        "/usr/bin/cosh-login",
        "/bin/cosh-login",
    ] {
        assert!(
            check_state(&[], &[account(shell)], false).is_err(),
            "{shell}"
        );
        assert!(check_state(&[], &[account(shell)], true).is_ok(), "{shell}");
        let mut other = account(shell);
        other.name = "bob".into();
        other.uid = 1002;
        other.gid = 1002;
        assert!(
            check_state(&[], &[account("/bin/bash"), other], false).is_err(),
            "{shell}"
        );
    }
    for shell in [
        "/bin/bash",
        "/usr/bin/zsh",
        "/usr/bin/cosh-wrapper",
        "/bin/cosh-login-wrapper",
        "/usr/local/bin/cosh",
        "/opt/bin/cosh-login",
    ] {
        assert!(
            check_state(&[], &[account(shell)], false).is_ok(),
            "{shell}"
        );
    }
}

#[test]
fn compatible_target_still_refuses_orphan_or_conflicting_records() {
    assert!(check_state(&[record(Phase::Applied)], &[], true).is_err());
    assert!(check_state(&[record(Phase::Applied)], &[account("/bin/zsh")], true).is_err());
    assert!(check_state(&[record(Phase::Conflict)], &[account(COSH_PATH)], true).is_err());
    assert!(check_state(&[record(Phase::Applied)], &[account(COSH_PATH)], true).is_ok());
}
