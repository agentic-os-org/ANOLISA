//! Decision and activation entrypoints retain the Service's single write boundary.

use super::{
    SkillGuardService, SkillRoot, check_locked, merge, poisoned, required_ledger, validate_skill,
};
use crate::UserDecision;
use crate::activation::{decision_value, publish, summary};
use crate::ledger::{Ledger, content::Content, storage::Directory};
use crate::{DecisionAction, GuardError, Manifest, ScanStatus, SigningIdentity, check_deadline};
use serde_json::{Value, json};
use std::time::Instant;

impl SkillGuardService {
    /// Signs a manual decision and publishes its trusted exposure target.
    ///
    /// # Errors
    /// Rejects untrusted latest records/snapshots, invalid decisions, paths and expired deadlines.
    /// A committed decision with incomplete publication reports activationPending in its result.
    pub fn decide(
        &self,
        root: &SkillRoot,
        action: DecisionAction,
        target_version: Option<&str>,
        reason: Option<&str>,
        deadline: Instant,
    ) -> Result<Value, GuardError> {
        if action == DecisionAction::Rollback {
            return self.rollback(root, target_version, reason, deadline);
        }
        if target_version.is_some() {
            return Err(GuardError::Invalid(
                "target version is only supported for rollback".into(),
            ));
        }
        self.with_locked_root(root, deadline, |directory, key| {
            self.recover_rollback(root, directory, key, deadline)?;
            validate_skill(directory)?;
            let ledger = required_ledger(directory)?;
            let mut manifest = ledger
                .latest(key, &root.identity, true, deadline)?
                .ok_or_else(|| {
                    GuardError::Integrity("cannot decide without a trusted latest version".into())
                })?;
            if manifest.scan_status == ScanStatus::None {
                return Err(GuardError::Invalid(
                    "cannot decide on an unscanned latest version".into(),
                ));
            }
            let content = ledger.snapshot(&manifest, deadline)?;
            manifest.user_decision = Some(UserDecision {
                action,
                target_version_id: None,
                reason: reason.map(str::to_owned),
            });
            merge(&mut manifest, Vec::new());
            key.sign_manifest(&mut manifest)?;
            ledger.commit(&manifest, &content, false, deadline, || Ok(()))?;
            let activation = refresh_locked(&ledger, directory, key, root, deadline);
            Ok(decision_payload(&manifest, &activation, true))
        })
    }

    /// Clears only the current trusted version's manual decision and refreshes exposure.
    ///
    /// # Errors
    /// Rejects untrusted records, unsafe storage and expired deadlines.
    pub fn clear_decision(&self, root: &SkillRoot, deadline: Instant) -> Result<Value, GuardError> {
        self.with_locked_root(root, deadline, |directory, key| {
            self.recover_rollback(root, directory, key, deadline)?;
            validate_skill(directory)?;
            let ledger = required_ledger(directory)?;
            let mut manifest = ledger
                .latest(key, &root.identity, true, deadline)?
                .ok_or_else(|| {
                    GuardError::Integrity("Skill has no trusted latest decision".into())
                })?;
            let content = ledger.snapshot(&manifest, deadline)?;
            manifest.user_decision = None;
            merge(&mut manifest, Vec::new());
            key.sign_manifest(&mut manifest)?;
            ledger.commit(&manifest, &content, false, deadline, || Ok(()))?;
            Ok(decision_payload(
                &manifest,
                &refresh_locked(&ledger, directory, key, root, deadline),
                false,
            ))
        })
    }

    /// Publishes the existing trusted selection without scanning or signing a new version.
    ///
    /// # Errors
    /// Reports unavailable keys, invalid roots and lock deadlines. Publication errors stay visible
    /// in activationPending/activationError because persistent business state may already exist.
    pub fn activate(&self, root: &SkillRoot, deadline: Instant) -> Result<Value, GuardError> {
        self.with_locked_root(root, deadline, |directory, key| {
            self.recover_rollback(root, directory, key, deadline)?;
            validate_skill(directory)?;
            let ledger = required_ledger(directory)?;
            Ok(refresh_locked(&ledger, directory, key, root, deadline))
        })
    }

    /// Shows latest/active versions, decisions and source consistency without publishing changes.
    ///
    /// # Errors
    /// Reports unavailable keys, invalid source paths and deadlines.
    pub fn show(&self, root: &SkillRoot, deadline: Instant) -> Result<Value, GuardError> {
        self.with_skill(root, deadline, |directory, key| {
            if !self
                .managed
                .lock()
                .map_err(|_| poisoned())?
                .contains(&root.identity)
            {
                return Ok(unmanaged(root));
            }
            let ledger = Ledger::open(directory, false)?;
            let status = check_locked(directory, key, root, deadline)?;
            let mut result = summary(ledger.as_ref(), key, root, &status, deadline)?;
            let latest = if status["versionId"].is_string() {
                ledger.as_ref().and_then(|store| {
                    store
                        .latest(key, &root.identity, true, deadline)
                        .ok()
                        .flatten()
                })
            } else {
                None
            };
            let active = match (ledger.as_ref(), result["activeVersionId"].as_str()) {
                (Some(store), Some(id)) => {
                    Some(store.version(id, key, &root.identity, deadline)?)
                }
                _ => None,
            };
            let matches = active
                .as_ref()
                .map(|manifest| {
                    Content::capture(directory, false, deadline)
                        .map(|c| c.hashes() == manifest.file_hashes)
                })
                .transpose()?;
            result["managed"] = json!(true);
            result["activationPolicy"] = json!("pass_warn_only");
            result["latest"] = manifest_summary(latest.as_ref(), status["status"].as_str());
            result["active"] = manifest_summary(active.as_ref(), None);
            result["rootMatchesActive"] = json!(matches);
            result["consistencyReason"] =
                super::display::consistency(&result, latest.as_ref(), active.as_ref(), matches);
            result["findings"] = status.get("findings").cloned().unwrap_or_else(|| json!([]));
            result["message"] = super::display::message(&result, &result["findings"]);
            result["warnings"] = result["message"]
                .as_str()
                .map_or_else(|| json!([]), |message| json!([message]));
            check_deadline(deadline)?;
            Ok(result)
        })
    }

    /// Recovers an interrupted rollback, repairs a complete version/latest split and republishes.
    ///
    /// # Errors
    /// Rejects damaged rollback backups, unsafe paths, keys and deadlines. Each caller must retain
    /// failures per Skill so an unrelated Skill can still complete startup reconciliation.
    pub fn reconcile(&self, root: &SkillRoot, deadline: Instant) -> Result<Value, GuardError> {
        self.with_locked_root(root, deadline, |directory, key| {
            let recovered = self.recover_rollback(root, directory, key, deadline)?;
            validate_skill(directory)?;
            let ledger = required_ledger(directory)?;
            let latest = ledger.latest(key, &root.identity, false, deadline).ok().flatten();
            let newest = ledger.newest(key, &root.identity, false, deadline)?;
            let mut repaired = false;
            if let Some(manifest) = newest
                && latest.as_ref() != Some(&manifest)
                && ledger.snapshot(&manifest, deadline).is_ok() {
                ledger.meta.write_atomic("latest.json", &serde_json::to_vec(&manifest)?, true)?;
                repaired = true;
            }
            cleanup_temporaries(&ledger, deadline)?;
            let activation = refresh_locked(&ledger, directory, key, root, deadline);
            Ok(json!({"canonicalSkillDir":root.identity,"skillName":root.identity.name(),
                "reconciled":true,"rollbackRecovered":recovered,"repairedLatest":repaired,"activation":activation}))
        })
    }
}

pub(super) fn refresh_locked(
    ledger: &Ledger,
    directory: &Directory,
    key: &SigningIdentity,
    root: &SkillRoot,
    deadline: Instant,
) -> Value {
    let selected = check_locked(directory, key, root, deadline)
        .and_then(|status| summary(Some(ledger), key, root, &status, deadline));
    match selected {
        Ok(selected) => publish(ledger, directory, root, &selected, deadline),
        Err(error) => json!({"canonicalSkillDir":root.identity,"skillName":root.identity.name(),
            "activationPending":true,"contractWritten":false,"activationError":error.to_string(),
            "activationXattr":{"name":crate::activation::ACTIVATION_XATTR,"written":false,"skipped":true}}),
    }
}

pub(super) fn decision_payload(
    manifest: &Manifest,
    activation: &Value,
    current_status: bool,
) -> Value {
    let mut result = json!({"status":"decided","canonicalSkillDir":manifest.canonical_skill_dir,
        "skillName":manifest.skill_name,"versionId":manifest.version_id,"scanStatus":manifest.scan_status,
        "manifestHash":manifest.manifest_hash,"userDecision":decision_value(manifest.user_decision.as_ref()),"activation":activation});
    if current_status {
        result["currentStatus"] = json!(manifest.scan_status);
    }
    result
}

fn manifest_summary(manifest: Option<&Manifest>, status: Option<&str>) -> Value {
    manifest.map_or(Value::Null, |manifest| {
        json!({"versionId":manifest.version_id,
        "status":status.map_or_else(|| json!(manifest.scan_status), |status| json!(status)),
        "scanStatus":manifest.scan_status,"manifestHash":manifest.manifest_hash,
        "userDecision":decision_value(manifest.user_decision.as_ref())})
    })
}

fn unmanaged(root: &SkillRoot) -> Value {
    json!({"canonicalSkillDir":root.identity,"skillName":root.identity.name(),"managed":false,
        "manageabilityReason":"canonical skill root is not configured in managedSkillDirs",
        "latestStatus":"unmanaged","latestVersionId":null,"activeVersionId":null,"target":null,
        "userDecision":null,"reasonCode":"unmanaged_skill_root","message":null,
        "activationPolicy":"pass_warn_only","latest":null,"active":null,"rootMatchesActive":null,
        "consistencyReason":"skill root is not managed by the current Skill Ledger daemon: canonical skill root is not configured in managedSkillDirs",
        "findings":[],"warnings":[]})
}

fn cleanup_temporaries(ledger: &Ledger, deadline: Instant) -> Result<(), GuardError> {
    for directory in [&ledger.meta, &ledger.versions] {
        for name in directory.names(deadline)? {
            let temporary = [".record-", ".snapshot-", ".pending-"]
                .iter()
                .any(|prefix| {
                    name.strip_prefix(prefix).is_some_and(|suffix| {
                        suffix.len() == 64 && suffix.bytes().all(|c| c.is_ascii_hexdigit())
                    })
                });
            if temporary {
                directory.remove_child(&name, deadline)?;
            }
        }
    }
    Ok(())
}
