//! v3.7 E06/E07 recent host event notices; E08-E10 stay diagnostics-only.
//! Wording reports past records, never current blocking or permanent lockup.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFinding, HealthFindingCategory, HealthMessageId, HealthSeverity,
};
use super::{evidence_ids, number, string_value};

const RECENT_WINDOW_SECONDS: f64 = 300.0;

pub(crate) fn host_event_findings(facts: &[HealthFact]) -> Vec<HealthFinding> {
    let mut findings = Vec::new();
    let hung_age = number(facts, "kernel.hung_task_latest_age_seconds");
    if hung_age.is_some_and(|age| age <= RECENT_WINDOW_SECONDS)
        && string_value(facts, "kernel.hung_task_event_identity").is_some()
    {
        let mut result = HealthFinding {
            id: "J30".to_string(),
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::RootCause,
            title_id: HealthMessageId::HealthFindingHungTask,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: evidence_ids(
                facts,
                &[
                    "kernel.hung_task_latest_age_seconds",
                    "kernel.hung_task_event_identity",
                    "kernel.hung_task_process",
                    "kernel.hung_task_pid",
                    "kernel.hung_task_blocked_seconds",
                    "kernel.hung_task_event_count_1h",
                    "kernel.boot_id",
                ],
            ),
            suggested_try_ids: Vec::new(),
        };
        if let Some(process) = string_value(facts, "kernel.hung_task_process") {
            result
                .detail_args
                .insert("process".into(), process.to_string());
        }
        if let Some(pid) = number(facts, "kernel.hung_task_pid") {
            result.detail_args.insert("pid".into(), format!("{pid:.0}"));
        }
        if let Some(seconds) = number(facts, "kernel.hung_task_blocked_seconds") {
            result
                .detail_args
                .insert("seconds".into(), format!("{seconds:.0}"));
        }
        findings.push(result);
    }
    let lockup_age = number(facts, "kernel.soft_lockup_latest_age_seconds");
    if lockup_age.is_some_and(|age| age <= RECENT_WINDOW_SECONDS)
        && string_value(facts, "kernel.soft_lockup_event_identity").is_some()
    {
        let mut result = HealthFinding {
            id: "J31".to_string(),
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::RootCause,
            title_id: HealthMessageId::HealthFindingSoftLockup,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: evidence_ids(
                facts,
                &[
                    "kernel.soft_lockup_latest_age_seconds",
                    "kernel.soft_lockup_event_identity",
                    "kernel.soft_lockup_cpu",
                    "kernel.soft_lockup_stuck_seconds",
                    "kernel.soft_lockup_event_count_1h",
                    "kernel.boot_id",
                ],
            ),
            suggested_try_ids: Vec::new(),
        };
        if let Some(cpu) = number(facts, "kernel.soft_lockup_cpu") {
            result.detail_args.insert("cpu".into(), format!("{cpu:.0}"));
        }
        if let Some(seconds) = number(facts, "kernel.soft_lockup_stuck_seconds") {
            result
                .detail_args
                .insert("seconds".into(), format!("{seconds:.0}"));
        }
        findings.push(result);
    }
    findings
}

#[cfg(test)]
mod tests;
