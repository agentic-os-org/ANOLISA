//! Startup recovery executes the same fenced rotation operation as administrator requests.

use super::*;
use asc_action_runtime::SecurityEventSink;
use asc_capability_skill_sec::SkillRoot;
use asc_security_events::SecurityEvent;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Mutex;

#[derive(Default)]
struct Events(Mutex<Vec<SecurityEvent>>);

impl SecurityEventSink for Events {
    fn write(&self, event: &SecurityEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[test]
fn startup_retries_unfinished_rotation_and_only_cleans_up_committed_rotation() {
    for committed in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let skill = temporary.path().join("skill");
        fs::create_dir(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "Safe fixture").unwrap();
        let service = SkillSecService::new(
            SkillSecConfig {
                state_dir: state.clone(),
                managed_skill_dirs: Vec::new(),
            },
            ScannerRegistry::default(),
        )
        .unwrap();
        service.initialize().unwrap();
        let root = SkillRoot::direct(&skill).unwrap();
        let deadline = || Instant::now() + Duration::from_secs(10);
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let before = service.key_status(deadline()).unwrap()["fingerprint"].clone();
        let replaced = if committed {
            service
                .rotate_keys(std::slice::from_ref(&root), 0, deadline())
                .unwrap();
            Some(service.key_status(deadline()).unwrap()["fingerprint"].clone())
        } else {
            None
        };
        // An uncleared intent can survive a crash before or after key replacement.
        let intent = state.join("key-rotation.json");
        fs::write(
            &intent,
            serde_json::to_vec(&json!({
                "previous_fingerprint":before, "skills":[root.identity]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&intent, fs::Permissions::from_mode(0o600)).unwrap();
        let parked = temporary.path().join("parked");
        fs::rename(&skill, &parked).unwrap();
        // A changed configuration must not change the interrupted rotation's identity set.
        let service = Arc::new(
            SkillSecService::new(
                SkillSecConfig {
                    state_dir: state.clone(),
                    managed_skill_dirs: vec![
                        SkillIdentity::new(temporary.path().join("extra")).unwrap(),
                    ],
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        let events = Arc::new(Events::default());
        let application = asc_daemon::skill_application(
            asc_action_runtime::testing::audit_finalizer(events.clone()),
            asc_capability_skill_sec::executor::SkillSecExecutor::new(service.clone()),
        );
        if !committed {
            assert!(
                recover_with_peer(
                    &service,
                    &application,
                    PeerCredentials::new(0, 0, std::process::id())
                )
                .is_err()
            );
            assert!(intent.exists());
            assert_eq!(
                service.key_status(deadline()).unwrap()["fingerprint"],
                before
            );
            assert!(matches!(
                service.check(&root, deadline()),
                Err(SkillSecError::RotationPending)
            ));
            fs::create_dir(&skill).unwrap();
            fs::write(skill.join("SKILL.md"), "Safe fixture").unwrap();
            fs::rename(parked.join(".skill-meta"), skill.join(".skill-meta")).unwrap();
            assert_ne!(
                fs::metadata(&skill).unwrap().ino(),
                fs::metadata(&parked).unwrap().ino()
            );
        }
        recover_with_peer(
            &service,
            &application,
            PeerCredentials::new(0, 0, std::process::id()),
        )
        .unwrap();
        let after = service.key_status(deadline()).unwrap();
        assert_eq!(after["rotationPending"], false);
        assert!(!intent.exists());
        if let Some(replaced) = replaced {
            assert_eq!(after["fingerprint"], replaced);
            assert!(!skill.exists());
        } else {
            assert_ne!(after["fingerprint"], before);
            assert_eq!(
                service.check(&root, deadline()).unwrap()["status"],
                "tampered"
            );
        }
        assert!(!events.0.lock().unwrap().is_empty());
    }
}

#[test]
fn startup_finalizes_preparation_failures_once_and_continues_with_the_next_skill() {
    use asc_capability_skill_sec::executor::{SkillEnvironment, SkillSecExecutor};
    struct Environment(&'static str);
    impl SkillEnvironment for Environment {
        fn resolve(
            &self,
            identity: &SkillIdentity,
            _: Instant,
        ) -> Result<SkillRoot, SkillSecError> {
            if identity.name() == "bad" {
                match self.0 {
                    "panic" => panic!("PRIVATE_PREPARATION_FAILURE"),
                    "timeout" => return Err(SkillSecError::Timeout),
                    _ => return Err(SkillSecError::Invalid("PRIVATE_INPUT".into())),
                }
            }
            SkillRoot::direct(identity.path())
        }
    }
    for mode in ["invalid", "timeout", "panic"] {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let service = Arc::new(
            SkillSecService::new(
                SkillSecConfig {
                    state_dir: state,
                    managed_skill_dirs: Vec::new(),
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        service.initialize().unwrap();
        for name in ["bad", "good"] {
            let path = temporary.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("SKILL.md"), "Safe fixture").unwrap();
            service
                .certify(
                    &SkillRoot::direct(path).unwrap(),
                    "fixture",
                    None,
                    &json!([]),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
        }
        let events = Arc::new(Events::default());
        let application = asc_daemon::skill_application(
            asc_action_runtime::testing::audit_finalizer(events.clone()),
            SkillSecExecutor::new(service.clone()).with_environment(Arc::new(Environment(mode))),
        );
        let context = asc_observability::Context::new().with_value(
            asc_observability::CompatibilityCorrelation {
                trace_id: Some("unrelated_rpc".into()),
                invocation_label: None,
            },
        );
        let _parent = context.attach();
        recover_with_peer(
            &service,
            &application,
            PeerCredentials::new(0, 0, std::process::id()),
        )
        .unwrap();
        let events = events.0.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].result, asc_security_events::EventResult::Failed);
        assert_eq!(
            events[1].result,
            asc_security_events::EventResult::Succeeded
        );
        assert!(
            events
                .iter()
                .all(|event| event.uid == 0 && event.pid == std::process::id())
        );
        assert!(events.iter().all(|event| event.trace_id.is_empty()));
        assert_eq!(
            asc_observability::snapshot()
                .compatibility
                .trace_id
                .as_deref(),
            Some("unrelated_rpc")
        );
        assert!(
            !serde_json::to_string(&*events)
                .unwrap()
                .contains("PRIVATE_")
        );
    }
}
