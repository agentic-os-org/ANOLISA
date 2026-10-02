use super::aggregate::{
    computed_entity_key, computed_finding_confidence, computed_suppression_key,
    finding_topic_from_findings, is_memory_hook, memory_hook_preference,
    recommended_skill_from_findings, severity_rank, AggregatedHookFinding,
};
use super::prelude::CommandBlock;
use crate::types::{BuiltinFactRecord, BuiltinFindingFacts, EvaluatedHookFinding, HookProvenance};

pub(crate) fn aggregate_hook_findings<T>(findings: Vec<T>) -> Vec<AggregatedHookFinding>
where
    T: Into<EvaluatedHookFinding>,
{
    let mut memory_findings: Vec<Vec<EvaluatedHookFinding>> = Vec::new();
    let mut aggregated = Vec::new();

    for finding in findings.into_iter().map(Into::into) {
        if is_memory_hook(&finding.hook_id) {
            if let Some(group) = memory_findings
                .iter_mut()
                .find(|group| same_owner(group[0].provenance(), finding.provenance()))
            {
                group.push(finding);
            } else {
                memory_findings.push(vec![finding]);
            }
        } else if let Some(existing) = aggregated
            .iter_mut()
            .find(|aggregate| should_merge_into_aggregate(aggregate, &finding))
        {
            merge_finding_into_aggregate(existing, finding);
        } else {
            aggregated.push(new_aggregated_hook_finding(finding, Vec::new()));
        }
    }

    for mut memory_group in memory_findings {
        memory_group.sort_by(|left, right| {
            severity_rank(right.severity)
                .cmp(&severity_rank(left.severity))
                .then_with(|| {
                    memory_hook_preference(&right.hook_id)
                        .cmp(&memory_hook_preference(&left.hook_id))
                })
        });
        let primary = memory_group.remove(0);
        aggregated.push(new_aggregated_hook_finding(primary, memory_group));
    }

    aggregated.sort_by(|left, right| {
        severity_rank(right.primary.severity).cmp(&severity_rank(left.primary.severity))
    });
    aggregated
}

fn should_merge_into_aggregate(
    aggregate: &AggregatedHookFinding,
    finding: &EvaluatedHookFinding,
) -> bool {
    if !same_owner(&aggregate.provenance, finding.provenance()) {
        return false;
    }
    let Some(skill) = finding.skill.as_deref() else {
        return false;
    };
    aggregate.recommended_skill.as_deref() == Some(skill)
        && aggregate.topic == finding_topic_from_findings(finding, &[])
}

fn merge_finding_into_aggregate(
    aggregate: &mut AggregatedHookFinding,
    finding: EvaluatedHookFinding,
) {
    let EvaluatedHookFinding {
        provenance,
        finding,
        builtin_facts,
    } = finding;
    if let Some(record) = builtin_fact_record(&provenance, builtin_facts) {
        aggregate.builtin_facts.push(record);
    }
    merge_provenance(&mut aggregate.provenance, provenance);
    if severity_rank(finding.severity) > severity_rank(aggregate.primary.severity) {
        let previous_primary = std::mem::replace(&mut aggregate.primary, finding);
        aggregate.related.insert(0, previous_primary);
    } else {
        aggregate.related.push(finding);
    }
    aggregate.recommended_skill =
        recommended_skill_from_findings(&aggregate.primary, &aggregate.related);
    aggregate.topic =
        finding_topic_from_findings(&aggregate.primary, &aggregate.related).to_string();
    aggregate.effective_severity = aggregate.primary.severity;
}

fn new_aggregated_hook_finding(
    primary: EvaluatedHookFinding,
    related: Vec<EvaluatedHookFinding>,
) -> AggregatedHookFinding {
    let EvaluatedHookFinding {
        provenance,
        finding: primary,
        builtin_facts,
    } = primary;
    let builtin_facts = builtin_fact_record(&provenance, builtin_facts)
        .into_iter()
        .collect();
    let recommended_skill = recommended_skill_from_findings(&primary, &[]);
    let topic = finding_topic_from_findings(&primary, &[]).to_string();
    let effective_severity = primary.severity;
    let mut aggregate = AggregatedHookFinding {
        provenance,
        builtin_facts,
        primary,
        related: Vec::new(),
        recommended_skill,
        topic,
        entity_key: String::new(),
        effective_severity,
        confidence: String::new(),
        suppression_key: String::new(),
    };
    for finding in related {
        merge_finding_into_aggregate(&mut aggregate, finding);
    }
    aggregate
}

fn builtin_fact_record(
    provenance: &HookProvenance,
    facts: Option<BuiltinFindingFacts>,
) -> Option<BuiltinFactRecord> {
    let facts = facts?;
    let HookProvenance::Builtin {
        producer_registration_ids,
    } = provenance
    else {
        return None;
    };
    let producer_registration_id = producer_registration_ids.iter().next()?.clone();
    Some(BuiltinFactRecord {
        producer_registration_id,
        facts,
    })
}

fn same_owner(left: &HookProvenance, right: &HookProvenance) -> bool {
    match (left, right) {
        (HookProvenance::Builtin { .. }, HookProvenance::Builtin { .. }) => true,
        (
            HookProvenance::External {
                registration_key: left,
            },
            HookProvenance::External {
                registration_key: right,
            },
        ) => left == right,
        _ => false,
    }
}

fn merge_provenance(target: &mut HookProvenance, incoming: HookProvenance) {
    match (target, incoming) {
        (
            HookProvenance::Builtin {
                producer_registration_ids: target,
            },
            HookProvenance::Builtin {
                producer_registration_ids: incoming,
            },
        ) => target.extend(incoming),
        (
            HookProvenance::External {
                registration_key: target,
            },
            HookProvenance::External {
                registration_key: incoming,
            },
        ) => debug_assert_eq!(*target, incoming),
        _ => unreachable!("aggregation owner checked before merge"),
    }
}

pub(crate) fn refresh_aggregate_metadata(
    block: &CommandBlock,
    aggregate: &mut AggregatedHookFinding,
) {
    aggregate.recommended_skill =
        recommended_skill_from_findings(&aggregate.primary, &aggregate.related);
    aggregate.topic =
        finding_topic_from_findings(&aggregate.primary, &aggregate.related).to_string();
    aggregate.entity_key = computed_entity_key(block, aggregate);
    aggregate.effective_severity = aggregate.primary.severity;
    aggregate.confidence = computed_finding_confidence(block, aggregate).to_string();
    aggregate.suppression_key = computed_suppression_key(block, aggregate);
}
