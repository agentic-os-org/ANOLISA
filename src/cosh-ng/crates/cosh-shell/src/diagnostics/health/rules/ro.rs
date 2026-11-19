//! v3.7 R10 read-only mount deviation: only configured expected_writable mounts
//! produce a notice; ordinary read-only mounts stay details-only.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFactValue, HealthFinding, HealthFindingCategory, HealthMessageId,
    HealthSeverity,
};
use super::evidence_ids;
use super::volume::{MemberJudgement, MemberLevel};

fn bool_value(facts: &[HealthFact], key: &str) -> Option<bool> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Bool(value) => Some(*value),
        _ => None,
    }
}

pub(crate) fn judge_ro(
    readonly: Option<bool>,
    expected_writable: bool,
    proven: bool,
) -> MemberJudgement {
    let valid = proven && readonly.is_some();
    let entry = if valid && readonly == Some(true) && expected_writable {
        Some(MemberLevel::Warning)
    } else {
        None
    };
    let exit = valid && readonly == Some(false);
    MemberJudgement { entry, exit, valid }
}

pub(crate) fn ro_judgement(facts: &[HealthFact], mount: &str) -> MemberJudgement {
    let prefix = format!("filesystem.mount.{mount}.");
    judge_ro(
        bool_value(facts, &format!("{prefix}volume_readonly")),
        bool_value(facts, &format!("{prefix}volume_expected_writable")) == Some(true),
        bool_value(facts, &format!("{prefix}volume_proven")) == Some(true),
    )
}

pub(crate) fn ro_findings(facts: &[HealthFact]) -> Vec<HealthFinding> {
    let mut mounts = Vec::new();
    for fact in facts {
        if let Some(mount) = fact
            .key
            .strip_prefix("filesystem.mount.")
            .and_then(|suffix| suffix.strip_suffix(".volume_proven"))
        {
            if !mounts.iter().any(|existing| existing == mount) {
                mounts.push(mount.to_string());
            }
        }
    }
    let mut findings = Vec::new();
    for mount in mounts {
        let judgement = ro_judgement(facts, &mount);
        if judgement.entry != Some(MemberLevel::Warning) {
            continue;
        }
        let prefix = format!("filesystem.mount.{mount}.");
        let mut result = HealthFinding {
            id: if mount == "/" {
                "J18".to_string()
            } else {
                format!("J18:{mount}")
            },
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::Anomaly,
            title_id: HealthMessageId::HealthFindingMountReadonly,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: evidence_ids(
                facts,
                &[
                    &format!("{prefix}volume_readonly"),
                    &format!("{prefix}volume_expected_writable"),
                    &format!("{prefix}volume_proven"),
                    &format!("{prefix}volume_device"),
                ],
            ),
            suggested_try_ids: Vec::new(),
        };
        result.detail_args.insert("mount".into(), mount.clone());
        result
            .detail_args
            .insert("members".into(), "readonly".into());
        findings.push(result);
    }
    findings
}

#[cfg(test)]
mod tests;
