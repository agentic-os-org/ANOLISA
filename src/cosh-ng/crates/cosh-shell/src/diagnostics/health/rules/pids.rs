//! v3.7 R06 current-cgroup task quota: probe-excluded snapshot ceiling and
//! conservative recovery. First batch stays diagnostics-only (A is M0-gated);
//! E12 rejection attribution lacks non-probe proof and also stays D.

use super::super::model::{HealthFact, HealthFactValue};
use super::volume::{MemberJudgement, MemberLevel};

/// Enforced health-path occupancy upper bound: one supervisor, one re-exec
/// worker, one sequential command child and one isolated cleanup child;
/// checks run sequentially and no reader threads are spawned. Formal
/// per-architecture registration belongs to the M0 release matrix.
pub(crate) const P_MAX_HEALTH_PATH_TIDS: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidsLimit {
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

fn bool_fact(facts: &[HealthFact], key: &str) -> Option<bool> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Bool(value) => Some(*value),
        _ => None,
    }
}

pub(crate) fn pids_limit(facts: &[HealthFact]) -> Option<PidsLimit> {
    if bool_fact(facts, "kernel.cgroup_pids_max_unlimited") == Some(true) {
        return Some(PidsLimit::Unlimited);
    }
    unsigned(facts, "kernel.cgroup_pids_max").map(PidsLimit::Finite)
}

/// Snapshot C excludes the proven probe bound; finite zero is an explicit
/// no-new-tasks quota fact without division. Recovery uses the conservative
/// raw headroom R_lo (probe not deducted) and never claims fork failure.
pub(crate) fn judge_pids(current: u64, max: PidsLimit, p_max: u64) -> MemberJudgement {
    match max {
        PidsLimit::Unlimited => MemberJudgement {
            entry: None,
            exit: true,
            valid: true,
        },
        PidsLimit::Finite(0) => MemberJudgement {
            entry: Some(MemberLevel::Critical),
            exit: false,
            valid: true,
        },
        PidsLimit::Finite(max) => {
            let adjusted = current.saturating_sub(p_max);
            let remaining = max.saturating_sub(current);
            let entry = if adjusted >= max {
                Some(MemberLevel::Critical)
            } else if u128::from(current) * 100 >= 90 * u128::from(max) && remaining <= 32 {
                Some(MemberLevel::Warning)
            } else {
                None
            };
            let exit = remaining > 0
                && (u128::from(current) * 100 <= 90 * u128::from(max) || remaining >= 8);
            MemberJudgement {
                entry,
                exit,
                valid: true,
            }
        }
    }
}

pub(crate) fn pids_judgement(facts: &[HealthFact]) -> MemberJudgement {
    match (
        unsigned(facts, "kernel.cgroup_pids_current"),
        pids_limit(facts),
    ) {
        (Some(current), Some(limit)) => judge_pids(current, limit, P_MAX_HEALTH_PATH_TIDS),
        _ => MemberJudgement::UNKNOWN,
    }
}

pub(crate) fn pids_level(facts: &[HealthFact]) -> Option<&'static str> {
    match pids_judgement(facts).entry {
        Some(MemberLevel::Critical) => Some("critical"),
        Some(MemberLevel::Warning) => Some("warning"),
        None => None,
    }
}

#[cfg(test)]
mod tests;
