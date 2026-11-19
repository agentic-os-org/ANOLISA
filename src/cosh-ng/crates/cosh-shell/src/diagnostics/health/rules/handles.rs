//! v3.7 R07 system file handles: approximate water level from native file-nr.
//! First batch stays diagnostics-only; no notice or state episode is created.

use super::super::model::{HealthFact, HealthFactValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HandlesJudgement {
    pub level: Option<&'static str>,
    pub exit: bool,
    pub valid: bool,
    pub zero_max: bool,
}

fn unsigned(facts: &[HealthFact], key: &str) -> Option<u64> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Unsigned(value) => Some(*value),
        HealthFactValue::Integer(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

/// H = allocated - unused, L = file-max, R = L - H; exact integer comparisons,
/// H>=L is legal (R=0). L=0 skips division and stays a separate D fact.
pub(crate) fn judge_handles(allocated: u64, unused: u64, max: u64) -> HandlesJudgement {
    if max == 0 {
        return HandlesJudgement {
            level: None,
            exit: false,
            valid: true,
            zero_max: true,
        };
    }
    let in_use = u128::from(allocated.saturating_sub(unused));
    let limit = u128::from(max);
    let remaining = limit.saturating_sub(in_use);
    let level = if remaining == 0 || (in_use * 100 >= 95 * limit && remaining <= 4096) {
        Some("critical")
    } else if in_use * 100 >= 80 * limit {
        Some("warning")
    } else {
        None
    };
    let exit = remaining > 0 && (in_use * 100 <= 90 * limit || remaining >= 8192);
    HandlesJudgement {
        level,
        exit,
        valid: true,
        zero_max: false,
    }
}

pub(crate) fn handles_level(facts: &[HealthFact]) -> Option<&'static str> {
    match (
        unsigned(facts, "host.file_nr_allocated"),
        unsigned(facts, "host.file_nr_unused"),
        unsigned(facts, "host.file_nr_max"),
    ) {
        (Some(allocated), Some(unused), Some(max)) => judge_handles(allocated, unused, max).level,
        _ => None,
    }
}

#[cfg(test)]
mod tests;
