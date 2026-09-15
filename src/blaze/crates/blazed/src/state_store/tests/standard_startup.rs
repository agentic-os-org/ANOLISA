// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn standard_startup_rejects_non_terminal_provider_records() {
    for transitions in [
        vec![],
        vec![SandboxState::Creating],
        vec![SandboxState::Creating, SandboxState::Running],
        vec![SandboxState::RecoveryRequired],
        vec![
            SandboxState::Creating,
            SandboxState::Running,
            SandboxState::Hibernating,
            SandboxState::Hibernated,
        ],
    ] {
        let temporary = tempfile::tempdir().expect("state root");
        let root = temporary.path().to_path_buf();
        let namespace = provider_namespace(Uuid::new_v4());
        let provider = StateStore::open_provider_namespace(root.clone(), &namespace)
            .expect("provider state owner");
        let mut active = instance();
        for state in transitions {
            if state == SandboxState::Hibernating {
                active
                    .begin_hibernate_operation()
                    .expect("hibernate intent");
            } else if state == SandboxState::Hibernated {
                active.backend_ownership = BackendOwnership::Stopped;
                active
                    .advance_hibernate_phase(
                        blaze_core::lifecycle::OperationPhase::HibernatePublished,
                    )
                    .expect("hibernate publication");
            }
            active.transition(state).expect("lifecycle transition");
        }
        provider.persist(&active).expect("provider record");
        let record = root
            .join(namespace)
            .join(active.id.to_string())
            .join(STATE_FILE);
        let before = std::fs::read(&record).expect("record before startup");
        drop(provider);

        let error = StateStore::open(root.clone())
            .expect_err("standard startup must reject non-terminal provider state");

        assert!(matches!(error, BlazeDaemonError::Conflict(message)
            if message.contains(&active.id.to_string())));
        assert_eq!(std::fs::read(record).expect("retained record"), before);
        assert!(!root.join(active.id.to_string()).exists());
    }
}

#[test]
fn standard_startup_accepts_clean_provider_namespaces_without_importing_them() {
    let temporary = tempfile::tempdir().expect("state root");
    let root = temporary.path().to_path_buf();
    let empty = provider_namespace(Uuid::new_v4());
    std::fs::create_dir(root.join(empty)).expect("empty namespace");
    let namespace = provider_namespace(Uuid::new_v4());
    let provider = StateStore::open_provider_namespace(root.clone(), &namespace)
        .expect("provider state owner");
    let mut destroyed = instance();
    destroyed
        .transition(SandboxState::Destroyed)
        .expect("destroy transition");
    provider
        .persist(&destroyed)
        .expect("terminal provider record");
    let record = root
        .join(namespace)
        .join(destroyed.id.to_string())
        .join(STATE_FILE);
    let before = std::fs::read(&record).expect("record before startup");
    drop(provider);
    let active_standard = instance();
    active_standard
        .persist(&root)
        .expect("standard record to recover");

    let standard = StateStore::open(root).expect("clean foreign state permits standard startup");
    let records = standard.scan().expect("standard inventory");

    assert_eq!(records.len(), 1);
    assert_eq!(records[&active_standard.id].state, SandboxState::Pending);
    assert!(!records.contains_key(&destroyed.id));
    assert_eq!(
        std::fs::read(record).expect("retained provider record"),
        before
    );
}

#[test]
fn standard_startup_preserves_uncertain_provider_records() {
    for corruption in ["json", "future-state", "uncleared-owner", "staging"] {
        let temporary = tempfile::tempdir().expect("state root");
        let root = temporary.path().to_path_buf();
        let namespace = provider_namespace(Uuid::new_v4());
        let provider = StateStore::open_provider_namespace(root.clone(), &namespace)
            .expect("provider state owner");
        let mut destroyed = instance();
        destroyed
            .transition(SandboxState::Destroyed)
            .expect("destroy transition");
        provider
            .persist(&destroyed)
            .expect("terminal provider record");
        drop(provider);
        let owner = root.join(namespace).join(destroyed.id.to_string());
        let record = owner.join(STATE_FILE);
        match corruption {
            "json" => std::fs::write(&record, b"{not-json").expect("corrupt record"),
            "future-state" => {
                let mut value = serde_json::to_value(&destroyed).expect("record JSON");
                value["state"] = "future-state".into();
                std::fs::write(&record, serde_json::to_vec(&value).expect("future JSON"))
                    .expect("unknown state record");
            }
            "uncleared-owner" => {
                destroyed.backend_ownership = BackendOwnership::Running;
                destroyed
                    .persist(owner.parent().expect("namespace"))
                    .expect("uncleared record");
            }
            "staging" => std::fs::write(owner.join(TEMP_STATE_FILE), b"uncertain")
                .expect("uncertain staging"),
            _ => unreachable!(),
        }
        let before = std::fs::read(&record).expect("record before startup");

        let error = StateStore::open(root)
            .expect_err("unproven provider cleanup must stop standard startup");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(std::fs::read(record).expect("record retained"), before);
        if corruption == "staging" {
            assert_eq!(
                std::fs::read(owner.join(TEMP_STATE_FILE)).expect("staging retained"),
                b"uncertain"
            );
        }
    }
}

#[test]
fn standard_startup_rejects_malformed_provider_namespaces() {
    for name in [".provider-state-vfuture", ".provider-state-v1-not-a-uuid"] {
        let temporary = tempfile::tempdir().expect("state root");
        std::fs::create_dir(temporary.path().join(name)).expect("malformed namespace");

        let error = StateStore::open(temporary.path().to_path_buf())
            .expect_err("malformed namespace must stop standard startup");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(temporary.path().join(name).is_dir());
    }
}

#[cfg(unix)]
#[test]
fn standard_startup_rejects_non_directory_provider_namespaces() {
    for symbolic_link in [false, true] {
        let temporary = tempfile::tempdir().expect("state root");
        let external = tempfile::tempdir().expect("external directory");
        let namespace = temporary.path().join(provider_namespace(Uuid::new_v4()));
        if symbolic_link {
            std::os::unix::fs::symlink(external.path(), &namespace).expect("namespace link");
        } else {
            std::fs::write(&namespace, b"not a directory").expect("namespace file");
        }

        let error = StateStore::open(temporary.path().to_path_buf())
            .expect_err("non-directory namespace must stop standard startup");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(std::fs::symlink_metadata(namespace).is_ok());
        assert_eq!(
            std::fs::read_dir(external.path())
                .expect("external contents")
                .count(),
            0
        );
    }
}

#[test]
fn standard_startup_rejects_an_independently_locked_provider_namespace() {
    let temporary = tempfile::tempdir().expect("state root");
    let namespace = temporary.path().join(provider_namespace(Uuid::new_v4()));
    std::fs::create_dir(&namespace).expect("provider namespace");
    let _owner = StateStore::open(namespace).expect("independent namespace owner");

    let error = StateStore::open(temporary.path().to_path_buf())
        .expect_err("a namespace owned elsewhere must stop standard startup");

    assert!(matches!(error, BlazeDaemonError::Conflict(_)));
}

#[test]
fn standard_startup_rejects_namespace_changes_during_inspection() {
    for replace in [false, true] {
        let temporary = tempfile::tempdir().expect("state root");
        let root = temporary.path().to_path_buf();
        let namespace = root.join(provider_namespace(Uuid::new_v4()));
        if replace {
            std::fs::create_dir(&namespace).expect("initial namespace");
        }

        let error = StateStore::open_with_lock_and_hook(root.clone(), true, || {
            if replace {
                std::fs::rename(&namespace, root.join("displaced-namespace"))?;
            }
            std::fs::create_dir(&namespace)?;
            Ok(())
        })
        .expect_err("added or replaced namespaces must stop standard startup");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(message)
            if message.contains("namespace set changed")));
    }
}

#[test]
fn standard_startup_rechecks_foreign_records_before_accepting_ownership() {
    let temporary = tempfile::tempdir().expect("state root");
    let root = temporary.path().to_path_buf();
    let namespace = root.join(provider_namespace(Uuid::new_v4()));
    std::fs::create_dir(&namespace).expect("initial empty namespace");
    let added = instance();

    let error = StateStore::open_with_lock_and_hook(root, true, || {
        added.persist(&namespace)?;
        Ok(())
    })
    .expect_err("a foreign record added during inspection must stop startup");

    assert!(matches!(error, BlazeDaemonError::Conflict(message)
        if message.contains(&added.id.to_string())));
    assert!(
        namespace
            .join(added.id.to_string())
            .join(STATE_FILE)
            .is_file()
    );
}
