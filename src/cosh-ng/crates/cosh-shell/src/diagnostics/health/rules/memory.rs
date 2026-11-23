//! v3.7 R03 host memory: dual-condition entry, single exit line, host qualification.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFactValue, HealthFinding, HealthFindingCategory, HealthMessageId,
    HealthSeverity,
};
use super::volume::{MemberJudgement, MemberLevel};
use super::{evidence_ids, number};

const MIB_KIB: u64 = 1024;

fn unsigned(facts: &[HealthFact], key: &str) -> Option<u64> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Unsigned(value) => Some(*value),
        HealthFactValue::Integer(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

pub(crate) fn judge_memory(
    available_kib: u64,
    total_kib: u64,
    host_qualified: bool,
) -> MemberJudgement {
    if !host_qualified || total_kib == 0 {
        return MemberJudgement {
            entry: None,
            exit: false,
            valid: false,
        };
    }
    let available = u128::from(available_kib);
    let total = u128::from(total_kib);
    let entry = if available * 100 < total * 5 && available_kib < 512 * MIB_KIB {
        Some(MemberLevel::Critical)
    } else if available * 100 < total * 10 && available_kib < 1024 * MIB_KIB {
        Some(MemberLevel::Warning)
    } else {
        None
    };
    let exit = available * 100 >= total * 8 || available_kib >= 768 * MIB_KIB;
    MemberJudgement {
        entry,
        exit,
        valid: true,
    }
}

pub(crate) fn memory_active(facts: &[HealthFact]) -> bool {
    facts.iter().any(|fact| fact.key == "memory.total_kib")
}

pub(crate) fn memory_judgement(facts: &[HealthFact], host_qualified: bool) -> MemberJudgement {
    match (
        unsigned(facts, "memory.available_kib"),
        unsigned(facts, "memory.total_kib"),
    ) {
        (Some(available), Some(total)) => judge_memory(available, total, host_qualified),
        _ => MemberJudgement {
            entry: None,
            exit: false,
            valid: false,
        },
    }
}

pub(crate) fn memory_findings(facts: &[HealthFact], host_qualified: bool) -> Vec<HealthFinding> {
    let judgement = memory_judgement(facts, host_qualified);
    if judgement.entry != Some(MemberLevel::Critical) {
        return Vec::new();
    }
    let mut result = HealthFinding {
        id: "J05".to_string(),
        severity: HealthSeverity::Critical,
        category: HealthFindingCategory::Anomaly,
        title_id: HealthMessageId::HealthFindingMemoryAvailableLow,
        detail_id: None,
        detail_args: BTreeMap::new(),
        evidence_fact_ids: evidence_ids(
            facts,
            &[
                "memory.available_kib",
                "memory.total_kib",
                "memory.available_mib",
                "memory.available_ratio",
            ],
        ),
        suggested_try_ids: Vec::new(),
    };
    if let Some(available_mib) = number(facts, "memory.available_mib") {
        result
            .detail_args
            .insert("available_mib".into(), format!("{available_mib:.0}"));
    }
    if let Some(ratio) = number(facts, "memory.available_ratio") {
        result
            .detail_args
            .insert("available_percent".into(), format!("{:.1}", ratio * 100.0));
    }
    vec![result]
}

#[cfg(test)]
mod tests;
