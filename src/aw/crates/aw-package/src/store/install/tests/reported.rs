//! Typed observations retain commit and recovery facts without inferring from messages.

use super::*;
use crate::{install_reported, uninstall_reported, Error, NoticeCode, RecoveryState};

#[test]
fn successful_mutations_report_commit_without_runtime_or_recovery_claims() {
    let f = Fixture::new();
    let report = install_reported(&f.combined, &f.prefix, &AtomicBool::new(false)).unwrap();
    assert!(report.committed);
    assert_eq!(report.recovery, RecoveryState::NotAttempted);
    assert!(report.notices.is_empty());
    fs::write(f.prefix.join("unknown"), "keep").unwrap();
    let report = uninstall_reported(&f.prefix, &AtomicBool::new(false)).unwrap();
    assert!(report.committed);
    assert_eq!(report.recovery, RecoveryState::NotAttempted);
    assert!(report.notices.is_empty());
    assert!(receipts(&f.prefix).unwrap().is_empty());
    assert_eq!(fs::read(f.prefix.join("unknown")).unwrap(), b"keep");
}

#[test]
fn preflight_failure_and_preexisting_cancellation_do_not_claim_recovery() {
    let f = Fixture::new();
    let failure = install_reported(&f.core, &f.prefix, &AtomicBool::new(true)).unwrap_err();
    assert!(matches!(failure.error, Error::Cancelled));
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::NotAttempted);
    assert!(failure.report.notices.is_empty());
    assert!(!f.prefix.exists());

    fs::write(f.core.join("payload/bin/aw"), "tampered").unwrap();
    let failure = install_reported(&f.core, &f.prefix, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(failure.error, Error::Invalid(_)));
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::NotAttempted);
    assert!(failure.report.notices.is_empty());
    assert!(!f.prefix.exists());
}

#[test]
fn failed_install_reports_completed_tracked_recovery_and_original_error() {
    for existing_core in [false, true] {
        let f = Fixture::new();
        let bundle = if existing_core {
            install(&f.core, &f.prefix).unwrap();
            &f.provider
        } else {
            &f.combined
        };
        let before = receipts(&f.prefix).unwrap();
        let failure = install_reported_using(
            bundle,
            &f.prefix,
            &AtomicBool::new(false),
            paths::lock,
            |_, _| Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into()),
        )
        .unwrap_err();
        assert!(
            matches!(failure.error, Error::Io(error) if error.raw_os_error() == Some(libc::ENOSPC))
        );
        assert!(!failure.report.committed);
        assert_eq!(failure.report.recovery, RecoveryState::Complete);
        assert!(failure.report.notices.is_empty());
        assert_eq!(receipts(&f.prefix).unwrap(), before);
        assert_eq!(f.prefix.exists(), existing_core);
    }
}

#[test]
fn install_cancellation_is_typed_only_when_a_check_observes_it() {
    let f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let failure = install_reported_using(
        &f.core,
        &f.prefix,
        &cancel,
        paths::lock,
        |source, output| {
            std::io::copy(&mut File::open(source)?, output)?;
            cancel.store(true, Ordering::Relaxed);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(matches!(failure.error, Error::Cancelled));
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::Complete);
    assert!(failure.report.notices.is_empty());
    assert!(!f.prefix.exists());
}

#[test]
fn failed_install_recovery_keeps_the_primary_failure_and_other_cleanup() {
    let f = Fixture::new();
    let failure = install_reported_using(
        &f.core,
        &f.prefix,
        &AtomicBool::new(false),
        paths::lock,
        |_, _| {
            fs::write(f.prefix.join("bin/unknown"), "keep")?;
            Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into())
        },
    )
    .unwrap_err();
    assert!(
        matches!(failure.error, Error::Io(error) if error.raw_os_error() == Some(libc::ENOSPC))
    );
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::Incomplete);
    assert!(failure.report.notices.iter().any(|notice| {
        notice.code == NoticeCode::RollbackRemoveFailed
            && notice.path == f.prefix.join("bin")
            && notice.io_kind.is_some()
    }));
    assert!(!f.prefix.join("bin/aw").exists());
    assert_eq!(fs::read(f.prefix.join("bin/unknown")).unwrap(), b"keep");
    assert!(f.prefix.join(".aw-install.lock").exists());
}

#[test]
fn failed_prefix_retirement_records_the_observation_without_deleting_foreign_lock() {
    let f = Fixture::new();
    let mut held = None;
    let failure = install_reported_using(
        &f.core,
        &f.prefix,
        &AtomicBool::new(false),
        |prefix| {
            held = Some(paths::lock(prefix)?);
            paths::lock(prefix)
        },
        |_, _| panic!("copy must not run without the lock"),
    )
    .unwrap_err();
    assert!(
        matches!(failure.error, Error::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::Incomplete);
    assert!(failure.report.notices.iter().any(|notice| {
        notice.code == NoticeCode::RollbackRetireFailed && notice.path == f.prefix
    }));
    assert_eq!(
        f.prefix.join(".aw-install.lock").metadata().unwrap().ino(),
        held.as_ref().unwrap().metadata().unwrap().ino()
    );
}

#[test]
fn interrupted_uninstall_reports_restored_payloads_and_receipts() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let before = receipts(&f.prefix).unwrap();
    let cancel = AtomicBool::new(false);
    let failure = uninstall_reported_using(
        &f.prefix,
        &cancel,
        |path| {
            fs::remove_file(path)?;
            cancel.store(true, Ordering::Relaxed);
            Ok(())
        },
        |path| fs::remove_dir_all(path),
    )
    .unwrap_err();
    assert!(matches!(failure.error, Error::Cancelled));
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::Complete);
    assert!(failure.report.notices.is_empty());
    assert_eq!(receipts(&f.prefix).unwrap(), before);
    verified_files(&f.prefix, &before).unwrap();
    assert!(!fs::read_dir(&f.prefix).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("uninstall-")));
}

#[test]
fn blocked_uninstall_restore_keeps_new_content_and_reports_actual_backup() {
    let f = Fixture::new();
    install(&f.core, &f.prefix).unwrap();
    let original = fs::read(f.prefix.join("bin/aw")).unwrap();
    let mut removed = false;
    let failure = uninstall_reported_using(
        &f.prefix,
        &AtomicBool::new(false),
        |path| {
            if removed {
                return Err(std::io::Error::from_raw_os_error(libc::EACCES));
            }
            fs::remove_file(path)?;
            fs::write(path, "concurrent user file")?;
            removed = true;
            Ok(())
        },
        |path| fs::remove_dir_all(path),
    )
    .unwrap_err();
    assert!(
        matches!(failure.error, Error::Io(error) if error.raw_os_error() == Some(libc::EACCES))
    );
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::Incomplete);
    assert!(failure.report.notices.iter().any(|notice| {
        notice.code == NoticeCode::RollbackRestoreFailed
            && notice.path == f.prefix.join("bin/aw")
            && notice.io_kind == Some(std::io::ErrorKind::AlreadyExists)
    }));
    let retained = failure
        .report
        .notices
        .iter()
        .find(|notice| notice.code == NoticeCode::BackupRetained)
        .unwrap();
    assert!(retained.path.is_dir());
    assert_eq!(fs::read(retained.path.join("0")).unwrap(), original);
    assert_eq!(
        fs::read(f.prefix.join("bin/aw")).unwrap(),
        b"concurrent user file"
    );
}

#[test]
fn committed_uninstall_with_cleanup_failure_is_success_with_a_notice() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let before = receipts(&f.prefix).unwrap();
    let report = uninstall_reported_using(
        &f.prefix,
        &AtomicBool::new(false),
        |path| fs::remove_file(path),
        |path| {
            fs::remove_file(path.join("0"))?;
            Err(std::io::Error::from_raw_os_error(libc::EACCES))
        },
    )
    .unwrap();
    assert!(report.committed);
    assert_eq!(report.recovery, RecoveryState::NotAttempted);
    assert_eq!(report.notices.len(), 1);
    let notice = &report.notices[0];
    assert_eq!(notice.code, NoticeCode::BackupCleanupFailed);
    assert_eq!(notice.io_kind, Some(std::io::ErrorKind::PermissionDenied));
    assert!(notice.path.is_dir());
    assert!(fs::read_dir(&notice.path).unwrap().next().is_some());
    assert!(receipts(&f.prefix).unwrap().is_empty());
    for component in before.values() {
        for name in component.files.keys() {
            assert!(!f.prefix.join(name).exists());
        }
    }
}

#[test]
fn cancellation_after_commit_does_not_reclassify_success() {
    let f = Fixture::new();
    install(&f.core, &f.prefix).unwrap();
    let cancel = AtomicBool::new(false);
    let report = uninstall_reported_using(
        &f.prefix,
        &cancel,
        |path| fs::remove_file(path),
        |path| {
            cancel.store(true, Ordering::Relaxed);
            fs::remove_dir_all(path)
        },
    )
    .unwrap();
    assert!(cancel.load(Ordering::Relaxed));
    assert!(report.committed);
    assert_eq!(report.recovery, RecoveryState::NotAttempted);
    assert!(report.notices.is_empty());
    assert!(receipts(&f.prefix).unwrap().is_empty());
}
