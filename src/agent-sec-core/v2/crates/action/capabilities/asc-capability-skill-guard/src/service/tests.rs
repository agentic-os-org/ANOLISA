use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::sync::mpsc;

pub(crate) fn fixture() -> (tempfile::TempDir, Arc<SkillGuardService>, SkillRoot) {
    let fixture = uninitialized_fixture();
    fixture.1.initialize().unwrap();
    fixture
}

fn uninitialized_fixture() -> (tempfile::TempDir, Arc<SkillGuardService>, SkillRoot) {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let state = base.join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let path = base.join("skill");
    fs::create_dir(&path).unwrap();
    fs::write(
        path.join("SKILL.md"),
        "---\nname: fixture\ndescription: fixture\n---\nSafe",
    )
    .unwrap();
    fs::write(path.join("run.sh"), "echo safe\n").unwrap();
    let service = Arc::new(
        SkillGuardService::new(
            GuardConfig {
                state_dir: state,
                managed_skill_dirs: vec![],
            },
            ScannerRegistry::default(),
        )
        .unwrap(),
    );
    (temporary, service, SkillRoot::direct(path).unwrap())
}

pub(crate) fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

#[test]
fn empty_skill_queries_succeed_without_initializing_keys_or_ledger() {
    for empty_ledger in [false, true] {
        let (_temporary, service, root) = uninitialized_fixture();
        if empty_ledger {
            fs::create_dir_all(root.io_dir.join(".skill-meta/versions")).unwrap();
        }
        for initialized in [false, true] {
            if initialized {
                service.initialize().unwrap();
            }
            let before = fs::read_dir(&service.config.state_dir).unwrap().count();
            let checked = service.check(&root, deadline()).unwrap();
            assert_eq!(checked["status"], "none");
            assert!(checked["versionId"].is_null());
            let audited = service.audit(&root, true, deadline()).unwrap();
            assert_eq!(audited["valid"], true);
            assert_eq!(audited["versions_checked"], 0);
            assert_eq!(audited["errors"], json!([]));
            assert_eq!(
                fs::read_dir(&service.config.state_dir).unwrap().count(),
                before
            );
            assert_eq!(root.io_dir.join(".skill-meta").exists(), empty_ledger);
            assert!(!root.io_dir.join(".skill-meta/latest.json").exists());
            if empty_ledger {
                assert_eq!(
                    fs::read_dir(root.io_dir.join(".skill-meta/versions"))
                        .unwrap()
                        .count(),
                    0
                );
            }
            assert_eq!(
                service.config.state_dir.join("signing-key.pk8").exists(),
                initialized
            );
            assert!(service.managed_skills().unwrap().is_empty());
        }
    }
}

#[test]
fn incomplete_ledger_without_a_key_is_not_an_unscanned_skill() {
    let (_temporary, service, root) = uninitialized_fixture();
    let meta = root.io_dir.join(".skill-meta");
    fs::create_dir(&meta).unwrap();
    assert!(service.check(&root, deadline()).is_err());
    assert!(service.audit(&root, true, deadline()).is_err());
    fs::create_dir(meta.join("versions")).unwrap();
    let orphan_snapshot = meta.join("versions/v000001.snapshot");
    fs::create_dir(&orphan_snapshot).unwrap();
    assert!(service.check(&root, deadline()).is_err());
    assert!(service.audit(&root, true, deadline()).is_err());
    fs::remove_dir(orphan_snapshot).unwrap();
    fs::write(meta.join("versions/v000001.json"), "{}").unwrap();
    assert!(service.check(&root, deadline()).is_err());
    assert!(service.audit(&root, true, deadline()).is_err());
    fs::remove_file(meta.join("versions/v000001.json")).unwrap();
    fs::write(meta.join("latest.json"), "{}").unwrap();
    assert!(service.check(&root, deadline()).is_err());
    assert!(service.audit(&root, true, deadline()).is_err());
    assert!(!service.config.state_dir.join("signing-key.pk8").exists());
    service.initialize().unwrap();
    assert_eq!(
        service.check(&root, deadline()).unwrap()["status"],
        "tampered"
    );
    assert_eq!(
        service.audit(&root, true, deadline()).unwrap()["valid"],
        false
    );
}

#[test]
fn existing_history_still_requires_a_valid_private_key() {
    let (_temporary, service, root) = fixture();
    service
        .certify(&root, "fixture", None, &json!([]), deadline())
        .unwrap();
    let key_path = service.config.state_dir.join("signing-key.pk8");
    let key = fs::read(&key_path).unwrap();
    fs::remove_file(&key_path).unwrap();
    assert!(service.check(&root, deadline()).is_err());
    assert!(service.audit(&root, true, deadline()).is_err());
    assert!(!key_path.exists());
    fs::write(&key_path, "invalid-key").unwrap();
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        service.check(&root, deadline()),
        Err(GuardError::Key)
    ));
    assert!(matches!(
        service.audit(&root, true, deadline()),
        Err(GuardError::Key)
    ));
    fs::write(&key_path, key).unwrap();
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(service.check(&root, deadline()).is_err());
    assert!(service.audit(&root, true, deadline()).is_err());
}

#[test]
fn empty_skill_queries_obey_skill_locks_and_rotation_fences() {
    let (_temporary, service, root) = uninitialized_fixture();
    let lock = service.skill_lock(&root.identity).unwrap();
    let guard = lock.lock().unwrap();
    assert!(matches!(
        service.check(&root, Instant::now() + Duration::from_millis(20)),
        Err(GuardError::Timeout)
    ));
    assert!(matches!(
        service.audit(&root, false, Instant::now() + Duration::from_millis(20)),
        Err(GuardError::Timeout)
    ));
    drop(guard);
    fs::write(
        service.config.state_dir.join("key-rotation.json"),
        serde_json::to_vec(&json!({"previous_fingerprint":"previous","skills":[]})).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        service.check(&root, deadline()),
        Err(GuardError::RotationPending)
    ));
    assert!(matches!(
        service.audit(&root, false, deadline()),
        Err(GuardError::RotationPending)
    ));
    assert!(!root.io_dir.join(".skill-meta").exists());
}

#[test]
fn ledger_snapshots_cannot_be_registered_as_skill_roots() {
    let (_temporary, service, root) = fixture();
    service
        .certify(&root, "fixture", None, &json!([]), deadline())
        .unwrap();
    let snapshot = root.io_dir.join(".skill-meta/versions/v000001.snapshot");
    for nested in [
        SkillRoot::direct(&snapshot).unwrap(),
        SkillRoot::resolved(root.identity.clone(), snapshot.clone()).unwrap(),
        SkillRoot::resolved(SkillIdentity::new(&snapshot).unwrap(), root.io_dir.clone()).unwrap(),
    ] {
        let result = service.scan(&nested, &ScanOptions::default(), deadline());
        assert!(
            matches!(result, Err(GuardError::Invalid(message)) if message.contains("reserved Ledger metadata"))
        );
    }
    assert!(!snapshot.join(".skill-meta").exists());
    assert_eq!(
        service.audit(&root, true, deadline()).unwrap()["valid"],
        true
    );
    let hidden_host = root.io_dir.with_file_name(".hermes").join("skills/example");
    fs::create_dir_all(&hidden_host).unwrap();
    fs::write(hidden_host.join("SKILL.md"), "safe").unwrap();
    assert_eq!(
        service
            .check(&SkillRoot::direct(hidden_host).unwrap(), deadline())
            .unwrap()["status"],
        "none"
    );
}

#[test]
fn retry_recovers_registration_failure_after_first_commit_without_a_new_version() {
    let (_temporary, service, root) = fixture();
    let registration = service.config.state_dir.join("managed-skills.json");
    // A directory blocks registration's atomic rename after the real Ledger commit succeeds.
    fs::create_dir(&registration).unwrap();
    assert!(
        service
            .scan(&root, &ScanOptions::default(), deadline())
            .is_err()
    );
    let latest_path = root.io_dir.join(".skill-meta/latest.json");
    let latest = fs::read(&latest_path).unwrap();
    assert_eq!(service.check(&root, deadline()).unwrap()["status"], "pass");
    assert!(service.managed_skills().unwrap().is_empty());
    assert!(!root.io_dir.join(".skill-meta/activation.json").exists());

    fs::remove_dir(&registration).unwrap();
    let config = service.config.clone();
    drop(service);
    let restarted = SkillGuardService::new(config.clone(), ScannerRegistry::default()).unwrap();
    // Startup only discovers persisted registrations. The unacknowledged request needs retry.
    assert!(restarted.managed_skills().unwrap().is_empty());
    let retried = restarted
        .scan(&root, &ScanOptions::default(), deadline())
        .unwrap();
    assert_eq!(retried["status"], "noop");
    assert_eq!(retried["newVersion"], false);
    assert_eq!(retried["versionId"], "v000001");
    assert_eq!(fs::read(&latest_path).unwrap(), latest);
    assert_eq!(
        restarted.managed_skills().unwrap(),
        vec![root.identity.clone()]
    );
    assert!(root.io_dir.join(".skill-meta/activation.json").is_file());
    drop(restarted);

    let restarted = SkillGuardService::new(config, ScannerRegistry::default()).unwrap();
    for identity in restarted.managed_skills().unwrap() {
        let result = restarted
            .reconcile(&SkillRoot::direct(identity.path()).unwrap(), deadline())
            .unwrap();
        assert_eq!(result["reconciled"], true);
        assert_eq!(result["repairedLatest"], false);
    }
    assert_eq!(
        restarted.audit(&root, true, deadline()).unwrap()["versions_checked"],
        1
    );
}

#[test]
fn staged_scan_ignores_later_live_edits_and_commit_recheck_rejects_them() {
    let (_temporary, service, root) = fixture();
    let directory = Directory::open(&root.io_dir).unwrap();
    fs::create_dir(root.io_dir.join("empty-directory")).unwrap();
    let content = Content::capture(&directory, false, deadline()).unwrap();
    let original = ScanTree::open(&root.io_dir, deadline()).unwrap();
    let (stage, tree) = content
        .scan_tree(&service.config.state_dir, &original, deadline())
        .unwrap();
    fs::write(root.io_dir.join("run.sh"), "rm -rf /\n").unwrap();
    let scanned = service
        .registry
        .scan_tree(&tree, Some(&["code-scanner".into()]), deadline())
        .unwrap();
    assert_eq!(scanned[0].status, ScanStatus::Pass);
    assert!(stage.path().join("empty-directory").is_dir());
    assert!(
        content
            .unchanged(&directory, &original, deadline())
            .is_err()
    );
    assert!(!root.io_dir.join(".skill-meta/latest.json").exists());
}

#[test]
fn link_changes_and_root_rebinding_are_detected_before_publication() {
    let (_temporary, _service, root) = fixture();
    let directory = Directory::open(&root.io_dir).unwrap();
    symlink("run.sh", root.io_dir.join("link")).unwrap();
    let content = Content::capture(&directory, false, deadline()).unwrap();
    let original = ScanTree::open(&root.io_dir, deadline()).unwrap();
    fs::remove_file(root.io_dir.join("link")).unwrap();
    symlink("/synthetic-missing", root.io_dir.join("link")).unwrap();
    assert!(
        content
            .unchanged(&directory, &original, deadline())
            .is_err()
    );
    fs::remove_file(root.io_dir.join("link")).unwrap();
    symlink("run.sh", root.io_dir.join("link")).unwrap();
    fs::rename(&root.io_dir, root.io_dir.with_file_name("old")).unwrap();
    fs::create_dir(&root.io_dir).unwrap();
    assert!(
        content
            .unchanged(&directory, &original, deadline())
            .is_err()
    );
}

#[test]
fn publication_rechecks_after_snapshot_write_and_cleans_unpublished_content() {
    let (_temporary, service, root) = fixture();
    service
        .with_skill(&root, deadline(), |directory, key| {
            let ledger = required_ledger(directory)?;
            let content = Content::capture(directory, false, deadline())?;
            let original = ScanTree::open(&root.io_dir, deadline())?;
            let mut manifest = Manifest::initial(root.identity.clone(), content.hashes());
            key.sign_manifest(&mut manifest)?;
            let result = ledger.commit(&manifest, &content, true, deadline(), || {
                let entries = ledger.versions.names(deadline())?;
                let temporary = entries
                    .iter()
                    .find(|name| name.starts_with(".snapshot-"))
                    .unwrap();
                assert_eq!(
                    fs::read_to_string(ledger.versions.path.join(temporary).join("run.sh"))
                        .unwrap(),
                    "echo safe\n"
                );
                fs::write(root.io_dir.join("run.sh"), "echo changed\n").unwrap();
                content.unchanged(directory, &original, deadline())
            });
            assert!(result.is_err());
            assert!(ledger.versions.names(deadline())?.is_empty());
            assert!(!ledger.meta.path.join("latest.json").exists());
            Ok(())
        })
        .unwrap();
}

#[test]
fn empty_directories_are_bounded_before_snapshot_creation() {
    let (_temporary, service, root) = fixture();
    for number in 0..10_001 {
        fs::create_dir(root.io_dir.join(format!("d{number}"))).unwrap();
    }
    let result = service.certify(
        &root,
        "fixture",
        None,
        &json!([]),
        Instant::now() + Duration::from_secs(60),
    );
    assert!(
        matches!(result, Err(GuardError::Invalid(message)) if message.contains("10000 directories"))
    );
    assert!(!root.io_dir.join(".skill-meta/latest.json").exists());
}

#[test]
#[ignore = "requires Linux root with CAP_CHOWN; run in the isolated acceptance container"]
fn exported_content_is_owned_and_writable_by_the_calling_user() {
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    let (_temporary, _service, root) = fixture();
    fs::create_dir(root.io_dir.join("nested")).unwrap();
    fs::write(root.io_dir.join("nested/data"), "fixture").unwrap();
    let content =
        Content::capture(&Directory::open(&root.io_dir).unwrap(), false, deadline()).unwrap();
    let output = root.io_dir.with_file_name("export");
    fs::create_dir(&output).unwrap();
    content
        .write_owned(&Directory::open(&output).unwrap(), deadline(), Some(10001))
        .unwrap();
    for path in ["", "SKILL.md", "run.sh", "nested", "nested/data"] {
        let metadata = fs::metadata(output.join(path)).unwrap();
        assert_eq!(metadata.uid(), 10001);
        assert_ne!(metadata.mode() & 0o200, 0);
    }
    // Restore only our two synthetic directory owners so reduced-DAC test containers can clean up.
    for path in ["nested", ""] {
        rustix::fs::chown(output.join(path), Some(rustix::process::Uid::ROOT), None).unwrap();
    }
}

#[test]
fn waiting_same_skill_expires_while_other_skills_can_proceed() {
    let (_temporary, service, root) = fixture();
    let other_path = root.io_dir.with_file_name("other");
    fs::create_dir(&other_path).unwrap();
    fs::write(other_path.join("SKILL.md"), "other").unwrap();
    let other = SkillRoot::direct(other_path).unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_service = Arc::clone(&service);
    let worker_root = root.clone();
    let worker = std::thread::spawn(move || {
        worker_service.with_skill(&worker_root, deadline(), |_, _| {
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        })
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let same = service.with_skill(&root, Instant::now() + Duration::from_millis(20), |_, _| {
        Ok(())
    });
    assert!(matches!(same, Err(GuardError::Timeout)));
    service
        .with_skill(&other, deadline(), |_, _| Ok(()))
        .unwrap();
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    // Expired identities do not accumulate in the daemon's lock registry.
    service.skill_lock(&root.identity).unwrap();
    assert_eq!(service.locks.lock().unwrap().len(), 1);
}
