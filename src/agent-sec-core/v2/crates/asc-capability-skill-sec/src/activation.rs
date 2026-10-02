//! Trusted exposure selection and the minimal file/xattr contract consumed by `SkillFS`.

use crate::ledger::Ledger;
use crate::ledger::storage::{Directory, nonce};
use crate::{
    DecisionAction, Manifest, ScanStatus, SigningIdentity, SkillRoot, SkillSecError, UserDecision,
    check_deadline, io_error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Instant;

/// `SkillFS` reads the same contract from activation.json and this directory xattr.
pub const ACTIVATION_XATTR: &str = "user.agent_sec.skill_ledger.activation";
/// Safe review stub used when no trusted eligible version is available.
pub const PENDING_TARGET: &str = ".skill-meta/versions/__pending_decision__.snapshot";
const PENDING_NAME: &str = "__pending_decision__.snapshot";

/// Wire contract retained independently of manifest schema and notify protocol versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivationContract {
    /// Exactly 1 for the current `SkillFS` activation contract.
    pub schema_version: u32,
    /// Relative trusted snapshot, safe review stub, or None to hide the Skill.
    pub target: Option<String>,
}

/// Eligibility state, independent of whether xattr publication succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExposureState {
    /// A verified snapshot is eligible for exposure.
    Active,
    /// Only the safe review stub is eligible.
    Pending,
    /// An explicit block hides the Skill.
    Hidden,
}

pub(crate) fn decision_value(decision: Option<&UserDecision>) -> Value {
    let Some(decision) = decision else {
        return Value::Null;
    };
    let mut value = json!({"action":decision.action});
    if let Some(version) = &decision.target_version_id {
        value["targetVersionId"] = json!(version);
    }
    if let Some(reason) = &decision.reason {
        value["reason"] = json!(reason);
    }
    value
}

enum Choice {
    Block(UserDecision),
    Decision(Manifest),
    Policy(Option<Manifest>),
}

fn choose(
    ledger: &Ledger,
    key: &SigningIdentity,
    root: &SkillRoot,
    deadline: Instant,
) -> Result<Choice, SkillSecError> {
    let ids = ledger.ids(deadline)?;
    let read = |id: &str| -> Result<Option<Manifest>, SkillSecError> {
        check_deadline(deadline)?;
        let record = ledger
            .version(id, key, &root.identity, deadline)
            .and_then(|manifest| ledger.snapshot(&manifest, deadline).map(|_| manifest));
        check_deadline(deadline)?;
        match record {
            Ok(manifest) => Ok(Some(manifest)),
            Err(SkillSecError::Timeout) => Err(SkillSecError::Timeout),
            Err(_) => Ok(None),
        }
    };
    let mut newer_trusted = false;
    let mut invalid = false;
    for id in ids.iter().rev() {
        let Some(manifest) = read(id)? else {
            invalid = true;
            continue;
        };
        let Some(decision) = &manifest.user_decision else {
            newer_trusted = true;
            continue;
        };
        if decision.action == DecisionAction::Block {
            if !newer_trusted {
                return Ok(Choice::Block(decision.clone()));
            }
            break;
        }
        if invalid {
            break;
        }
        return Ok(Choice::Decision(manifest));
    }
    // Re-read only when policy fallback is needed, rather than retaining every findings body.
    for id in ids.iter().rev() {
        if let Some(manifest) = read(id)?
            && matches!(manifest.scan_status, ScanStatus::Pass | ScanStatus::Warn)
        {
            return Ok(Choice::Policy(Some(manifest)));
        }
    }
    check_deadline(deadline)?;
    Ok(Choice::Policy(None))
}

pub(crate) fn summary(
    ledger: Option<&Ledger>,
    key: &SigningIdentity,
    root: &SkillRoot,
    status: &Value,
    deadline: Instant,
) -> Result<Value, SkillSecError> {
    let choice = match ledger {
        Some(ledger) => choose(ledger, key, root, deadline)?,
        None => Choice::Policy(None),
    };
    let latest_status = status["status"].as_str().unwrap_or("none");
    let latest_id = status["versionId"].as_str();
    let (active, decision, reason, message) = match choice {
        Choice::Block(decision) => (None, Some(decision), "user_block".to_owned(), None),
        Choice::Decision(manifest) => {
            let reason = match manifest.user_decision.as_ref().map(|d| d.action) {
                Some(DecisionAction::AlwaysAllow) => "user_always_allow",
                Some(DecisionAction::Rollback) => "user_rollback",
                _ => "user_allow",
            };
            (
                Some(manifest.version_id),
                manifest.user_decision,
                reason.into(),
                None,
            )
        }
        Choice::Policy(manifest) => {
            let active = manifest.map(|m| m.version_id);
            let (reason, message) = policy_message(latest_status, latest_id, active.as_deref());
            (active, None, reason.into(), message)
        }
    };
    let target = active
        .as_ref()
        .map(|id| format!(".skill-meta/versions/{id}.snapshot"))
        .or_else(|| (decision.is_none() && message.is_some()).then(|| PENDING_TARGET.into()));
    let state = if active.is_some() {
        ExposureState::Active
    } else if target.is_some() {
        ExposureState::Pending
    } else {
        ExposureState::Hidden
    };
    Ok(
        json!({"canonicalSkillDir":root.identity,"skillName":root.identity.name(),
        "latestStatus":latest_status,"latestVersionId":latest_id,"activeVersionId":active,
        "target":target,"userDecision":decision_value(decision.as_ref()),"reasonCode":reason,
        "message":message,"exposureState":state}),
    )
}

fn policy_message(
    latest: &str,
    latest_id: Option<&str>,
    active: Option<&str>,
) -> (&'static str, Option<String>) {
    if matches!(latest, "pass" | "warn") && active.is_some() && active == latest_id {
        return ("normal", None);
    }
    let version = latest_id.unwrap_or("none");
    let (reason, prefix) = match latest {
        "pass" | "warn" => (
            "tampered",
            format!("Latest {latest} snapshot is not a trusted activation target"),
        ),
        "drifted" => (
            "root_drift",
            format!("Current skill root drifted from latest signed version {version}"),
        ),
        "tampered" => (
            "tampered",
            format!("Latest skill metadata is tampered for version {version}"),
        ),
        _ => (
            if active.is_some() {
                "latest_risk_fallback_to_previous"
            } else {
                "latest_risk_pending_decision"
            },
            format!("Latest skill status is {latest} for version {version}"),
        ),
    };
    let suffix = active.map_or_else(|| "active skill is a safe review stub pending user decision. Review with 'agent-sec-cli skill-ledger show' or 'agent-sec-cli skill-ledger export', then choose with 'agent-sec-cli skill-ledger decide'.".into(), |id| format!("active version is {id}."));
    (reason, Some(format!("{prefix}; {suffix}")))
}

pub(crate) fn publish(
    ledger: &Ledger,
    directory: &Directory,
    root: &SkillRoot,
    summary: &Value,
    deadline: Instant,
) -> Value {
    publish_with(ledger, directory, root, summary, deadline, |bytes| {
        rustix::fs::fsetxattr(
            &directory.file,
            ACTIVATION_XATTR,
            bytes,
            rustix::fs::XattrFlags::empty(),
        )
        .map_err(|e| io_error(&root.io_dir, e))
    })
}

fn publish_with(
    ledger: &Ledger,
    directory: &Directory,
    root: &SkillRoot,
    summary: &Value,
    deadline: Instant,
    set_xattr: impl FnOnce(&[u8]) -> Result<(), SkillSecError>,
) -> Value {
    let target = summary["target"].as_str().map(str::to_owned);
    let contract = ActivationContract {
        schema_version: 1,
        target,
    };
    let mut result = summary.clone();
    result["schemaVersion"] = json!(1);
    result["status"] = summary["latestStatus"].clone();
    result["policy"] = json!("pass_warn_only");
    result["activationPath"] = json!(root.identity.path().join(".skill-meta/activation.json"));
    result["contractWritten"] = json!(false);
    result["activationXattr"] =
        json!({"name":ACTIVATION_XATTR,"written":false,"available":true,"skipped":true});
    let outcome = (|| {
        check_deadline(deadline)?;
        directory.verify_path()?;
        if contract.target.as_deref() == Some(PENDING_TARGET) {
            write_pending(ledger, root, deadline)?;
        }
        let bytes = serde_json::to_vec(&contract)?;
        ledger.meta.write_atomic("activation.json", &bytes, true)?;
        result["contractWritten"] = json!(true);
        result["activationXattr"]["skipped"] = json!(false);
        set_xattr(&bytes)?;
        result["activationXattr"]["written"] = json!(true);
        if contract.target.as_deref() != Some(PENDING_TARGET) {
            ledger.versions.remove_child(PENDING_NAME, deadline)?;
        }
        Ok::<_, SkillSecError>(())
    })();
    if let Err(error) = outcome {
        // The signed business decision may already be committed. Report publication separately
        // so a caller can inspect/retry it and startup reconciliation can finish the same intent.
        result["activationPending"] = json!(true);
        result["activationError"] = json!(error.to_string().replace(
            root.io_dir.to_string_lossy().as_ref(),
            root.identity.path().to_string_lossy().as_ref()
        ));
    } else {
        result["activationPending"] = json!(false);
    }
    result
}

fn write_pending(
    ledger: &Ledger,
    root: &SkillRoot,
    deadline: Instant,
) -> Result<(), SkillSecError> {
    let temporary = nonce(".pending-")?;
    let staging = ledger.versions.fresh_child(&temporary)?;
    let name = serde_json::to_string(root.identity.name())?;
    let text = format!(
        "---\nname: {name}\ndescription: Skill requires manual review before use\n---\n# Pending Skill Ledger Review\n\nThis is a safe placeholder. The real skill version is not exposed because it is not currently eligible for exposure and no trusted fallback version is available.\n\nRun `agent-sec-cli skill-ledger show <skill_dir>` to inspect the status. Use `scan` or `certify` to establish trust, or `skill-ledger decide` to approve or block a trusted version.\n"
    );
    let result = (|| {
        staging.write_new("SKILL.md", text.as_bytes(), 0o644)?;
        staging.sync()?;
        ledger.versions.remove_child(PENDING_NAME, deadline)?;
        ledger.versions.rename_child(&temporary, PENDING_NAME)
    })();
    if result.is_err() {
        let _ = ledger.versions.remove_child(
            &temporary,
            Instant::now() + std::time::Duration::from_secs(5),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::{deadline, fixture};

    #[test]
    fn large_findings_history_keeps_policy_fallback_and_audit_complete() {
        use crate::ledger::content::Content;
        use crate::{Finding, KeyStore};
        use std::time::Duration;
        let (_temporary, service, root) = fixture();
        let deadline = Instant::now() + Duration::from_secs(120);
        service
            .certify(&root, "fixture", None, &json!([]), deadline)
            .unwrap();
        let directory = Directory::open(&root.io_dir).unwrap();
        let ledger = Ledger::open(&directory, false).unwrap().unwrap();
        let key = KeyStore::open(&service.config.state_dir)
            .unwrap()
            .load()
            .unwrap();
        let content = Content::capture(&directory, false, deadline).unwrap();
        let mut manifest = ledger
            .latest(&key, &root.identity, true, deadline)
            .unwrap()
            .unwrap();
        manifest.scans[0].status = ScanStatus::Deny;
        manifest.scans[0].findings.push(Finding {
            rule: "synthetic-large-finding".into(),
            level: ScanStatus::Deny,
            message: "x".repeat(256 * 1024),
            file: None,
            line: None,
            metadata: std::collections::BTreeMap::new(),
        });
        manifest.scan_status = ScanStatus::Deny;
        for number in 2..=64 {
            manifest.previous_version_id = Some(manifest.version_id.clone());
            manifest.previous_manifest_signature =
                Some(manifest.signature.as_ref().unwrap().value.clone());
            manifest.version_id = format!("v{number:06}");
            key.sign_manifest(&mut manifest).unwrap();
            ledger
                .commit(&manifest, &content, true, deadline, || Ok(()))
                .unwrap();
        }
        match choose(&ledger, &key, &root, deadline).unwrap() {
            Choice::Policy(Some(selected)) => assert_eq!(selected.version_id, "v000001"),
            _ => panic!("older eligible version must remain selectable"),
        }
        let audit = ledger.audit(&key, &root.identity, true, deadline).unwrap();
        assert_eq!(audit["valid"], true);
        assert_eq!(audit["versions_checked"], 64);
        std::fs::write(ledger.versions.path.join("v000032.json"), "invalid record").unwrap();
        match choose(&ledger, &key, &root, deadline).unwrap() {
            Choice::Policy(Some(selected)) => assert_eq!(selected.version_id, "v000001"),
            _ => panic!("invalid history must not erase policy fallback"),
        }
        let audit = ledger.audit(&key, &root.identity, false, deadline).unwrap();
        assert_eq!(audit["valid"], false);
        assert_eq!(audit["versions_checked"], 64);
        assert!(
            audit["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|error| error["versionId"] == "v000032")
        );
    }

    #[test]
    fn xattr_failure_preserves_committed_decision_and_reports_retry() {
        let (_temporary, service, root) = fixture();
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let directory = Directory::open(&root.io_dir).unwrap();
        let ledger = Ledger::open(&directory, false).unwrap().unwrap();
        let before = std::fs::read(ledger.meta.path.join("latest.json")).unwrap();
        let selected =
            json!({"latestStatus":"pass","target":".skill-meta/versions/v000001.snapshot"});
        let result = publish_with(&ledger, &directory, &root, &selected, deadline(), |_| {
            Err(SkillSecError::Integrity("injected xattr failure".into()))
        });
        assert_eq!(result["contractWritten"], true);
        assert_eq!(result["activationPending"], true);
        assert_eq!(result["activationXattr"]["written"], false);
        assert!(
            result["activationError"]
                .as_str()
                .unwrap()
                .contains("injected xattr failure")
        );
        assert_eq!(
            std::fs::read(ledger.meta.path.join("latest.json")).unwrap(),
            before
        );
        let retried = service.activate(&root, deadline()).unwrap();
        assert_eq!(retried["activationPending"], false);
        assert_eq!(retried["activationXattr"]["written"], true);
    }
}
