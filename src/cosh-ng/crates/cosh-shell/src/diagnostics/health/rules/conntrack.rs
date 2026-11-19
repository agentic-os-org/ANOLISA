//! v3.7 R09 conntrack capacity: only proven critical water levels become W
//! notices under network_capacity_notices; ordinary 90% stays diagnostics-only.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFactValue, HealthFinding, HealthFindingCategory, HealthMessageId,
    HealthSeverity,
};
use super::volume::{MemberJudgement, MemberLevel};
use super::{evidence_ids, number};

fn unsigned(facts: &[HealthFact], key: &str) -> Option<u64> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Unsigned(value) => Some(*value),
        HealthFactValue::Integer(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

/// max=0 is a proven limit removal: it exits any old member without reporting
/// full or healthy. Positive max uses exact integer ratio and headroom lines.
pub(crate) fn judge_conntrack(count: u64, max: u64) -> MemberJudgement {
    if max == 0 {
        return MemberJudgement {
            entry: None,
            exit: true,
            valid: true,
        };
    }
    let count = u128::from(count);
    let max = u128::from(max);
    let remaining = max.saturating_sub(count);
    let entry = if count >= max || (count * 100 >= 98 * max && remaining <= 1024) {
        Some(MemberLevel::Critical)
    } else if count * 100 >= 90 * max {
        Some(MemberLevel::Warning)
    } else {
        None
    };
    let exit = count < max && (count * 100 <= 90 * max || remaining >= 2048);
    MemberJudgement {
        entry,
        exit,
        valid: true,
    }
}

pub(crate) fn conntrack_judgement(facts: &[HealthFact]) -> MemberJudgement {
    match (
        unsigned(facts, "host.conntrack_count"),
        unsigned(facts, "host.conntrack_max"),
    ) {
        (Some(count), Some(max)) => judge_conntrack(count, max),
        _ => MemberJudgement::UNKNOWN,
    }
}

pub(crate) fn conntrack_level(facts: &[HealthFact]) -> Option<&'static str> {
    match conntrack_judgement(facts).entry {
        Some(MemberLevel::Critical) => Some("critical"),
        Some(MemberLevel::Warning) => Some("warning"),
        None => None,
    }
}

pub(crate) fn conntrack_findings(facts: &[HealthFact]) -> Vec<HealthFinding> {
    let judgement = conntrack_judgement(facts);
    if judgement.entry != Some(MemberLevel::Critical) {
        return Vec::new();
    }
    let mut result = HealthFinding {
        id: "J23".to_string(),
        severity: HealthSeverity::Warning,
        category: HealthFindingCategory::Anomaly,
        title_id: HealthMessageId::HealthFindingConntrackCapacity,
        detail_id: None,
        detail_args: BTreeMap::new(),
        evidence_fact_ids: evidence_ids(
            facts,
            &[
                "host.conntrack_count",
                "host.conntrack_max",
                "host.conntrack_netns_id",
                "host.conntrack_level",
            ],
        ),
        suggested_try_ids: Vec::new(),
    };
    if let Some(count) = number(facts, "host.conntrack_count") {
        result
            .detail_args
            .insert("count".into(), format!("{count:.0}"));
    }
    if let Some(max) = number(facts, "host.conntrack_max") {
        result.detail_args.insert("max".into(), format!("{max:.0}"));
    }
    vec![result]
}

#[cfg(test)]
mod tests;
