//! System trust lifecycle; rotation withdraws exposure before replacing the only verification key.

use super::{SkillGuardService, SkillRoot, poisoned, required_ledger};
use crate::activation::publish;
use crate::ledger::storage::{Directory, MAX_RECORD_BYTES, missing};
use crate::{GuardError, KeyStore, SkillIdentity, check_deadline};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{RwLockWriteGuard, TryLockError};
use std::time::{Duration, Instant};

const ROTATION_INTENT: &str = "key-rotation.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RotationIntent {
    previous_fingerprint: String,
    skills: Vec<SkillIdentity>,
}

impl SkillGuardService {
    pub(super) fn generation_write(
        &self,
        deadline: Instant,
    ) -> Result<RwLockWriteGuard<'_, ()>, GuardError> {
        loop {
            check_deadline(deadline)?;
            match self.generation.try_write() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
                Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
            }
        }
    }

    pub(super) fn require_no_rotation(&self, deadline: Instant) -> Result<(), GuardError> {
        if self.rotation_intent(deadline)?.is_some() {
            Err(GuardError::RotationPending)
        } else {
            Ok(())
        }
    }

    fn rotation_intent(&self, deadline: Instant) -> Result<Option<RotationIntent>, GuardError> {
        match Directory::open(&self.config.state_dir)?.read(
            ROTATION_INTENT,
            MAX_RECORD_BYTES,
            deadline,
        ) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if missing(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Reports system key readiness without creating keys or trusting caller HOME.
    ///
    /// # Errors
    /// Reports inaccessible private state and malformed recovery metadata.
    pub fn key_status(&self, deadline: Instant) -> Result<Value, GuardError> {
        check_deadline(deadline)?;
        let mut result = match KeyStore::open(&self.config.state_dir)?.load() {
            Ok(key) => {
                json!({"initialized":true,"fingerprint":key.fingerprint(),"encrypted":false,"keyringSize":0})
            }
            Err(error) if missing(&error) => {
                json!({"initialized":false,"fingerprint":null,"encrypted":false,"keyringSize":0})
            }
            Err(error) => {
                json!({"initialized":false,"fingerprint":null,"error":error.to_string(),"encrypted":false,"keyringSize":0})
            }
        };
        result["rotationPending"] = json!(self.rotation_intent(deadline)?.is_some());
        Ok(result)
    }

    /// Returns identities to resolve for a new or interrupted rotation.
    /// A committed key replacement needs only intent cleanup, without filesystem mappings.
    ///
    /// # Errors
    /// Reports inaccessible state, malformed recovery metadata and unavailable signing keys.
    pub fn rotation_skills(&self, deadline: Instant) -> Result<Vec<SkillIdentity>, GuardError> {
        if let Some(intent) = self.rotation_intent(deadline)? {
            let current = KeyStore::open(&self.config.state_dir)?.load()?;
            return Ok(if current.fingerprint() == intent.previous_fingerprint {
                intent.skills
            } else {
                Vec::new()
            });
        }
        self.managed_skills()
    }

    /// Rotates system trust as root, or resumes a previously authorized interrupted rotation.
    ///
    /// `roots` must cover exactly the new or pending rotation's Skill set with fresh mappings.
    /// The persisted intent is private daemon state, never imported from a Skill or RPC body.
    ///
    /// # Errors
    /// Rejects non-root callers, missing mappings, unfinished rollback, unsafe paths, publication
    /// failure and deadlines. Once an intent exists, ordinary Ledger access remains blocked.
    pub fn rotate_keys(
        &self,
        roots: &[SkillRoot],
        caller_uid: u32,
        deadline: Instant,
    ) -> Result<Value, GuardError> {
        if caller_uid != 0 {
            return Err(GuardError::PermissionDenied);
        }
        let _generation = self.generation_write(deadline)?;
        let store = KeyStore::open(&self.config.state_dir)?;
        let current = store.load()?;
        let state = Directory::open(&self.config.state_dir)?;
        let pending = self.rotation_intent(deadline)?;
        let recovering = pending.is_some();
        let intent = match pending {
            Some(intent) => intent,
            None => RotationIntent {
                previous_fingerprint: current.fingerprint(),
                skills: self.managed_skills()?,
            },
        };
        // A new fingerprint proves replacement committed; never rotate a second time on recovery.
        if current.fingerprint() == intent.previous_fingerprint {
            let expected: BTreeSet<_> = intent.skills.iter().cloned().collect();
            let provided: BTreeSet<_> = roots.iter().map(|r| r.identity.clone()).collect();
            if expected != provided || provided.len() != roots.len() {
                return Err(GuardError::Invalid(
                    "rotation requires exactly one resolved mapping for every recorded Skill"
                        .into(),
                ));
            }
            if state
                .names(deadline)?
                .iter()
                .any(|name| name.starts_with(".rollback-") && name.strip_suffix(".json").is_some())
            {
                return Err(GuardError::Invalid(
                    "reconcile pending rollback before rotating keys".into(),
                ));
            }
            if !recovering {
                let bytes = serde_json::to_vec(&intent)?;
                if bytes.len() as u64 > MAX_RECORD_BYTES {
                    return Err(GuardError::Invalid(
                        "rotation intent exceeds metadata limit".into(),
                    ));
                }
                state.write_atomic(ROTATION_INTENT, &bytes, true)?;
            }
            for root in roots {
                check_deadline(deadline)?;
                let directory = Directory::open(&root.io_dir)?;
                let ledger = required_ledger(&directory)?;
                let withdrawn = publish(
                    &ledger,
                    &directory,
                    root,
                    &json!({"latestStatus":"tampered","target":null,"activeVersionId":null,"exposureState":"hidden","reasonCode":"key_rotation"}),
                    deadline,
                );
                if withdrawn["activationPending"] == true {
                    return Err(GuardError::Integrity(
                        "cannot rotate until every managed activation is withdrawn".into(),
                    ));
                }
            }
            check_deadline(deadline)?;
            store.replace()?;
        }
        let fingerprint = store.load()?.fingerprint();
        state.remove_child(ROTATION_INTENT, deadline)?;
        Ok(
            json!({"rotated":true,"keyFingerprint":fingerprint,"previousKeyRetained":false,"trustRebuildRequired":true}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::{deadline, fixture};
    use std::fs;
    use std::os::unix::fs::MetadataExt as _;

    #[test]
    fn only_root_rotates_and_old_records_require_new_trust() {
        let (_temporary, service, root) = fixture();
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let before = service.key_status(deadline()).unwrap()["fingerprint"].clone();
        assert!(matches!(
            service.rotate_keys(std::slice::from_ref(&root), 1000, deadline()),
            Err(GuardError::PermissionDenied)
        ));
        assert_eq!(service.check(&root, deadline()).unwrap()["status"], "pass");
        service
            .rotate_keys(std::slice::from_ref(&root), 0, deadline())
            .unwrap();
        assert_ne!(
            service.key_status(deadline()).unwrap()["fingerprint"],
            before
        );
        assert_eq!(
            fs::metadata(service.config.state_dir.join("signing-key.pk8"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        let activation: Value = serde_json::from_slice(
            &fs::read(root.io_dir.join(".skill-meta/activation.json")).unwrap(),
        )
        .unwrap();
        assert!(activation["target"].is_null());
        assert_eq!(
            service.check(&root, deadline()).unwrap()["status"],
            "tampered"
        );
        let result = service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        assert_eq!(result["versionId"], "v000002");
        assert_eq!(service.check(&root, deadline()).unwrap()["status"], "pass");
        assert!(
            service
                .export(&root, "v000001", &root.io_dir, 0, deadline())
                .is_err()
        );
    }

    #[test]
    fn rotation_intent_fences_mutations_and_resumes_once_after_key_commit() {
        let (_temporary, service, root) = fixture();
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let store = KeyStore::open(&service.config.state_dir).unwrap();
        let intent = RotationIntent {
            previous_fingerprint: store.load().unwrap().fingerprint(),
            skills: vec![root.identity.clone()],
        };
        let state = Directory::open(&service.config.state_dir).unwrap();
        state
            .write_atomic(ROTATION_INTENT, &serde_json::to_vec(&intent).unwrap(), true)
            .unwrap();
        assert_eq!(
            service.key_status(deadline()).unwrap()["rotationPending"],
            true
        );
        assert!(matches!(
            service.scan(&root, &crate::ScanOptions::default(), deadline()),
            Err(GuardError::RotationPending)
        ));
        assert!(matches!(
            service.initialize_with_deadline(deadline()),
            Err(GuardError::RotationPending)
        ));
        assert_eq!(
            service.rotation_skills(deadline()).unwrap(),
            vec![root.identity.clone()]
        );
        for mappings in [
            Vec::new(),
            vec![root.clone(), root.clone()],
            vec![SkillRoot::direct(root.io_dir.with_file_name("unrelated")).unwrap()],
        ] {
            assert!(matches!(
                service.rotate_keys(&mappings, 0, deadline()),
                Err(GuardError::Invalid(_))
            ));
            assert_eq!(
                store.load().unwrap().fingerprint(),
                intent.previous_fingerprint
            );
        }
        // Simulate a crash after replacement, before deleting the private intent.
        let replaced = store.replace().unwrap().fingerprint();
        assert!(service.rotation_skills(deadline()).unwrap().is_empty());
        service.rotate_keys(&[], 0, deadline()).unwrap();
        assert_eq!(
            service.key_status(deadline()).unwrap()["fingerprint"],
            replaced
        );
        assert_eq!(
            service.key_status(deadline()).unwrap()["rotationPending"],
            false
        );
    }

    #[test]
    fn failed_withdrawal_keeps_old_key_and_a_recoverable_intent() {
        let (_temporary, service, root) = fixture();
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let before = service.key_status(deadline()).unwrap()["fingerprint"].clone();
        let parked = root.io_dir.with_file_name("parked");
        fs::rename(&root.io_dir, &parked).unwrap();
        assert!(
            service
                .rotate_keys(std::slice::from_ref(&root), 0, deadline())
                .is_err()
        );
        assert_eq!(
            service.key_status(deadline()).unwrap()["fingerprint"],
            before
        );
        assert_eq!(
            service.key_status(deadline()).unwrap()["rotationPending"],
            true
        );
        fs::rename(parked, &root.io_dir).unwrap();
        service.rotate_keys(&[root], 0, deadline()).unwrap();
        assert_ne!(
            service.key_status(deadline()).unwrap()["fingerprint"],
            before
        );
        assert_eq!(
            service.key_status(deadline()).unwrap()["rotationPending"],
            false
        );
    }
}
