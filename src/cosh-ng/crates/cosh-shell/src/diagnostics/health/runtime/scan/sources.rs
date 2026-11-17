//! Per-source dispatch plan derived from the static registry.

use std::collections::BTreeSet;
use std::time::Duration;

use super::super::super::plan::{definition, FinishPolicy, SampleSource, SOURCES};

pub(super) const SPAWN_ADMIT_COST: Duration = Duration::from_millis(15);
pub(super) const MIN_SOURCE_WORK: Duration = Duration::from_millis(2);
const REUSE_CLEANUP_RESERVE: Duration = Duration::from_millis(5);
const RETIRE_CLEANUP_RESERVE: Duration = Duration::from_millis(10);

pub(super) struct SourcePlan {
    pub(super) source: SampleSource,
    pub(super) check_ids: Vec<String>,
    pub(super) budget: Duration,
    pub(super) finish: FinishPolicy,
}

pub(super) struct AwaitedTask {
    pub(super) task_id: u64,
    pub(super) finish: FinishPolicy,
    pub(super) deadline: super::HealthDeadline,
    pub(super) work_deadline: super::HealthDeadline,
}

pub(super) fn build_plan(enabled: &BTreeSet<String>, passive: bool) -> Vec<SourcePlan> {
    let mut plan = Vec::new();
    for source in SOURCES {
        let check_ids: Vec<String> = enabled
            .iter()
            .filter(|id| definition(id).is_some_and(|check| check.source == source.source))
            .cloned()
            .collect();
        if check_ids.is_empty() {
            continue;
        }
        plan.push(SourcePlan {
            source: source.source,
            check_ids,
            budget: Duration::from_millis(if passive {
                source.passive_budget_ms
            } else if source.source == SampleSource::Services {
                800
            } else {
                750
            }),
            finish: source.finish_policy,
        });
    }
    plan
}

pub(super) fn cleanup_reservation(finish: FinishPolicy, budget: Duration) -> Duration {
    (match finish {
        FinishPolicy::ReuseOnSuccess => REUSE_CLEANUP_RESERVE,
        FinishPolicy::RetireAfterSource => RETIRE_CLEANUP_RESERVE,
    })
    .min(budget / 4)
}

#[cfg(test)]
library_tests! {
mod tests {
    use super::*;

    #[test]
    fn on_demand_sources_keep_their_own_budget() {
        let enabled = BTreeSet::from(["filesystem.root".into(), "kernel.oom".into()]);
        assert!(build_plan(&enabled, false)
            .iter()
            .all(|source| source.budget == Duration::from_millis(750)));
        assert!(build_plan(&enabled, true)
            .iter()
            .all(|source| source.budget < Duration::from_millis(750)));
    }

    #[test]
    fn short_source_cleanup_never_exceeds_a_quarter_of_its_budget() {
        for ms in [1, 10, 30, 100] {
            let budget = Duration::from_millis(ms);
            for finish in [
                FinishPolicy::ReuseOnSuccess,
                FinishPolicy::RetireAfterSource,
            ] {
                assert!(cleanup_reservation(finish, budget) <= budget / 4);
            }
        }
    }
}
}
