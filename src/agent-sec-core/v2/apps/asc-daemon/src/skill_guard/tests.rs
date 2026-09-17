//! Startup recovery executes the same fenced rotation operation as administrator requests.

use super::*;
use asc_action_runtime::SecurityEventSink;
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
        let service = SkillGuardService::new(
            GuardConfig {
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
            SkillGuardService::new(
                GuardConfig {
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
        let finalizer = Finalizer::new(events.clone());
        if !committed {
            assert!(recover(&service, finalizer.clone(), None).is_err());
            assert!(intent.exists());
            assert_eq!(
                service.key_status(deadline()).unwrap()["fingerprint"],
                before
            );
            assert!(matches!(
                service.check(&root, deadline()),
                Err(GuardError::RotationPending)
            ));
            fs::create_dir(&skill).unwrap();
            fs::write(skill.join("SKILL.md"), "Safe fixture").unwrap();
            fs::rename(parked.join(".skill-meta"), skill.join(".skill-meta")).unwrap();
            assert_ne!(
                fs::metadata(&skill).unwrap().ino(),
                fs::metadata(&parked).unwrap().ino()
            );
        }
        recover(&service, finalizer, None).unwrap();
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
