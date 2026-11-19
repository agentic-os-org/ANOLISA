//! v3.7 E02-E05 findings from proven filesystem event chains: exact age
//! tiers, mapped devices only; degraded chains stay diagnostics-only.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFinding, HealthFindingCategory, HealthMessageId, HealthSeverity,
};
use super::{evidence_ids, number, string_value};

struct MechanismRule {
    prefix: &'static str,
    id: &'static str,
    title: HealthMessageId,
    critical_age: Option<u64>,
    warning_age: u64,
}

const MECHANISMS: &[MechanismRule] = &[
    MechanismRule {
        prefix: "ext4_forced_ro",
        id: "J24",
        title: HealthMessageId::HealthFindingExt4Error,
        critical_age: Some(300),
        warning_age: 3600,
    },
    MechanismRule {
        prefix: "ext4_error",
        id: "J25",
        title: HealthMessageId::HealthFindingExt4Error,
        critical_age: None,
        warning_age: 300,
    },
    MechanismRule {
        prefix: "jbd2_abort",
        id: "J26",
        title: HealthMessageId::HealthFindingJournalError,
        critical_age: Some(300),
        warning_age: 3600,
    },
    MechanismRule {
        prefix: "jbd2_flush",
        id: "J27",
        title: HealthMessageId::HealthFindingJournalError,
        critical_age: None,
        warning_age: 300,
    },
    MechanismRule {
        prefix: "xfs_shutdown",
        id: "J28",
        title: HealthMessageId::HealthFindingXfsShutdown,
        critical_age: Some(300),
        warning_age: 3600,
    },
    MechanismRule {
        prefix: "block_io",
        id: "J29",
        title: HealthMessageId::HealthFindingBlockIoError,
        critical_age: None,
        warning_age: 300,
    },
];

pub(crate) fn fs_event_findings(facts: &[HealthFact]) -> Vec<HealthFinding> {
    let mut findings = Vec::new();
    for rule in MECHANISMS {
        let age_key = format!("kernel.fs_{}_age_seconds", rule.prefix);
        let Some(age) = number(facts, &age_key) else {
            continue;
        };
        let mount_key = format!("kernel.fs_{}_mount", rule.prefix);
        if string_value(facts, &mount_key).is_none() {
            continue;
        }
        let severity = if rule.critical_age.is_some_and(|limit| age <= limit as f64) {
            HealthSeverity::Critical
        } else if age <= rule.warning_age as f64 {
            HealthSeverity::Warning
        } else {
            continue;
        };
        let evidence = evidence_ids(
            facts,
            &[
                &age_key,
                &mount_key,
                &format!("kernel.fs_{}_device", rule.prefix),
                &format!("kernel.fs_{}_dev_t", rule.prefix),
                &format!("kernel.fs_{}_identity", rule.prefix),
                &format!("kernel.fs_{}_epoch", rule.prefix),
                &format!("kernel.fs_{}_operation", rule.prefix),
                "kernel.boot_id",
                "kernel.fs_event_source",
            ],
        );
        let mut result = HealthFinding {
            id: rule.id.to_string(),
            severity,
            category: HealthFindingCategory::RootCause,
            title_id: rule.title,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: evidence,
            suggested_try_ids: Vec::new(),
        };
        if let Some(mount) = string_value(facts, &mount_key) {
            result.detail_args.insert("mount".into(), mount.to_string());
        }
        if let Some(device) = string_value(facts, &format!("kernel.fs_{}_device", rule.prefix)) {
            result
                .detail_args
                .insert("device".into(), device.to_string());
        }
        result
            .detail_args
            .insert("age_seconds".into(), format!("{age:.0}"));
        if let Some(operation) =
            string_value(facts, &format!("kernel.fs_{}_operation", rule.prefix))
        {
            result
                .detail_args
                .insert("operation".into(), operation.to_string());
        }
        findings.push(result);
    }
    findings
}

#[cfg(test)]
mod tests;
