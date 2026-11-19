//! v3.7 R05 current-cgroup memory quota: independent max/high branches and a
//! same-domain PSI combination notice. Quota alone stays diagnostics-only.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFactValue, HealthFinding, HealthFindingCategory, HealthMessageId,
    HealthSeverity,
};
use super::psi::hundredths;
use super::volume::{MemberJudgement, MemberLevel};
use super::{evidence_ids, number};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaLimit {
    Unlimited,
    Finite(u64),
}

fn unsigned(facts: &[HealthFact], key: &str) -> Option<u64> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Unsigned(value) => Some(*value),
        HealthFactValue::Integer(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

fn string_fact(facts: &[HealthFact], key: &str) -> Option<String> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn bool_fact(facts: &[HealthFact], key: &str) -> Option<bool> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Bool(value) => Some(*value),
        _ => None,
    }
}

fn limit(facts: &[HealthFact], name: &str) -> Option<QuotaLimit> {
    if bool_fact(facts, &format!("kernel.cgroup_memory_{name}_unlimited")) == Some(true) {
        return Some(QuotaLimit::Unlimited);
    }
    unsigned(facts, &format!("kernel.cgroup_memory_{name}_bytes")).map(QuotaLimit::Finite)
}

/// Finite zero is a critical quota fact without division; unlimited is a valid
/// exit; positive limits use exact integer ratio and headroom comparisons.
pub(crate) fn judge_max(current: u64, max: QuotaLimit) -> MemberJudgement {
    match max {
        QuotaLimit::Unlimited => MemberJudgement {
            entry: None,
            exit: true,
            valid: true,
        },
        QuotaLimit::Finite(0) => MemberJudgement {
            entry: Some(MemberLevel::Critical),
            exit: false,
            valid: true,
        },
        QuotaLimit::Finite(max) => {
            let current = u128::from(current);
            let max = u128::from(max);
            let remaining = max.saturating_sub(current);
            let entry = if current * 100 >= 95 * max && remaining <= u128::from(128 * MIB) {
                Some(MemberLevel::Critical)
            } else if current * 100 >= 90 * max && remaining <= u128::from(512 * MIB) {
                Some(MemberLevel::Warning)
            } else {
                None
            };
            let exit = current * 100 <= 90 * max || remaining >= u128::from(256 * MIB);
            MemberJudgement {
                entry,
                exit,
                valid: true,
            }
        }
    }
}

/// high=0 only participates while current>0; current=0 then exits the branch
/// even when PSI stays high. Positive high uses current>=high entry and the
/// 90% exit line without floating point.
pub(crate) fn judge_high(current: u64, high: QuotaLimit) -> MemberJudgement {
    match high {
        QuotaLimit::Unlimited => MemberJudgement {
            entry: None,
            exit: true,
            valid: true,
        },
        QuotaLimit::Finite(0) => {
            if current == 0 {
                MemberJudgement {
                    entry: None,
                    exit: true,
                    valid: true,
                }
            } else {
                MemberJudgement {
                    entry: Some(MemberLevel::Warning),
                    exit: false,
                    valid: true,
                }
            }
        }
        QuotaLimit::Finite(high) => MemberJudgement {
            entry: (current >= high).then_some(MemberLevel::Warning),
            exit: u128::from(current) * 10 <= u128::from(high) * 9,
            valid: true,
        },
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QuotaJudgement {
    pub current: Option<u64>,
    pub max: MemberJudgement,
    pub high: MemberJudgement,
    pub same_domain: bool,
    pub psi_full: Option<(f64, f64)>,
    pub cgroup_id: Option<String>,
}

pub(crate) fn quota_judgement(facts: &[HealthFact]) -> QuotaJudgement {
    let current = unsigned(facts, "kernel.cgroup_memory_current_bytes");
    let max = match (current, limit(facts, "max")) {
        (Some(current), Some(limit)) => judge_max(current, limit),
        _ => MemberJudgement::UNKNOWN,
    };
    let high = match (current, limit(facts, "high")) {
        (Some(current), Some(limit)) => judge_high(current, limit),
        _ => MemberJudgement::UNKNOWN,
    };
    let cgroup_id = string_fact(facts, "kernel.current_cgroup_id").filter(|id| !id.is_empty());
    let quota_id = string_fact(facts, "kernel.cgroup_memory_object_id");
    let psi_id = string_fact(facts, "kernel.cgroup_psi_object_id");
    let same_domain = match (&cgroup_id, quota_id, psi_id) {
        (Some(id), Some(quota_id), Some(psi_id)) => *id == quota_id && *id == psi_id,
        _ => false,
    };
    let psi_full = match (
        number(facts, "kernel.cgroup_psi_memory_full_avg10"),
        number(facts, "kernel.cgroup_psi_memory_full_avg60"),
    ) {
        (Some(avg10), Some(avg60)) => Some((avg10, avg60)),
        _ => None,
    };
    QuotaJudgement {
        current,
        max,
        high,
        same_domain,
        psi_full,
        cgroup_id,
    }
}

pub(crate) fn psi_combination_high(psi_full: Option<(f64, f64)>) -> bool {
    match psi_full {
        Some((avg10, avg60)) => matches!(
            (hundredths(avg10), hundredths(avg60)),
            (Some(avg10), Some(avg60)) if avg10 >= 2000 && avg60 >= 1000
        ),
        None => false,
    }
}

pub(crate) fn psi_combination_recovery(psi_full: Option<(f64, f64)>) -> bool {
    match psi_full {
        Some((avg10, avg60)) => matches!(
            (hundredths(avg10), hundredths(avg60)),
            (Some(avg10), Some(avg60)) if avg10 < 500 && avg60 < 100
        ),
        None => false,
    }
}

pub(crate) fn combo_entry(judgement: &QuotaJudgement) -> bool {
    judgement.same_domain
        && (judgement.max.entry == Some(MemberLevel::Critical) || judgement.high.entry.is_some())
        && psi_combination_high(judgement.psi_full)
}

pub(crate) fn quota_findings(facts: &[HealthFact]) -> Vec<HealthFinding> {
    let judgement = quota_judgement(facts);
    if !combo_entry(&judgement) {
        return Vec::new();
    }
    let max_entry = judgement.max.entry == Some(MemberLevel::Critical);
    let high_entry = judgement.high.entry.is_some();
    let branches = match (max_entry, high_entry) {
        (true, true) => "max+high",
        (true, false) => "max",
        (false, true) => "high",
        _ => return Vec::new(),
    };
    let mut evidence = evidence_ids(
        facts,
        &[
            "kernel.cgroup_memory_current_bytes",
            "kernel.cgroup_memory_max_bytes",
            "kernel.cgroup_memory_max_unlimited",
            "kernel.cgroup_memory_high_bytes",
            "kernel.cgroup_memory_high_unlimited",
            "kernel.cgroup_memory_object_id",
            "kernel.cgroup_psi_memory_full_avg10",
            "kernel.cgroup_psi_memory_full_avg60",
            "kernel.cgroup_psi_memory_full_total",
            "kernel.cgroup_psi_object_id",
            "kernel.current_cgroup_path",
        ],
    );
    if let Some(fact) = super::fact(facts, "kernel.cgroup_psi_memory_level") {
        evidence.push(fact.id.clone());
    }
    let mut result = HealthFinding {
        id: "J19".to_string(),
        severity: HealthSeverity::Warning,
        category: HealthFindingCategory::Anomaly,
        title_id: HealthMessageId::HealthFindingCgroupMemoryPressure,
        detail_id: None,
        detail_args: BTreeMap::new(),
        evidence_fact_ids: evidence,
        suggested_try_ids: Vec::new(),
    };
    result
        .detail_args
        .insert("branches".into(), branches.into());
    if let Some(path) = string_fact(facts, "kernel.current_cgroup_path") {
        result.detail_args.insert("cgroup".into(), path);
    }
    if let (Some(current), Some(QuotaLimit::Finite(max))) = (judgement.current, limit(facts, "max"))
    {
        if max > 0 {
            result.detail_args.insert(
                "percent".into(),
                (u128::from(current) * 100 / u128::from(max)).to_string(),
            );
            result.detail_args.insert(
                "remaining_mib".into(),
                (max.saturating_sub(current) / MIB).to_string(),
            );
        } else {
            result.detail_args.insert("max_zero".into(), "true".into());
        }
    }
    if let Some((avg10, avg60)) = judgement.psi_full {
        result
            .detail_args
            .insert("avg10".into(), format!("{avg10:.2}"));
        result
            .detail_args
            .insert("avg60".into(), format!("{avg60:.2}"));
    }
    vec![result]
}

#[cfg(test)]
mod tests;
