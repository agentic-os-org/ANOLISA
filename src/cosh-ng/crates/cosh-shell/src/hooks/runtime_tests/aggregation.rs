use super::*;
use crate::types::{
    BuiltinFindingFacts, EvaluatedHookFinding, HighMemoryProcessFacts, HookProvenance,
    MemoryPressureFacts, MetricsConfidence, ProcessMemoryFact,
};
use std::collections::BTreeSet;

#[test]
fn aggregation_combines_memory_pressure_and_process() {
    let findings = vec![
        finding("high-memory-process", FindingSeverity::Info),
        finding("memory-pressure", FindingSeverity::Warning),
    ];
    let aggregated = aggregate_hook_findings(findings);

    assert_eq!(aggregated.len(), 1);
    assert_eq!(aggregated[0].primary.hook_id, "memory-pressure");
    assert_eq!(aggregated[0].related[0].hook_id, "high-memory-process");
    assert_eq!(
        display_for_aggregate(&block(0), &aggregated[0], false),
        RuntimeHookDisplay::Consultation
    );
}

#[test]
fn aggregation_combines_same_topic_and_recommended_skill() {
    let findings = vec![
        external_finding(
            "docker-health",
            FindingSeverity::Warning,
            Some("container-analysis"),
        ),
        external_finding(
            "docker-cgroup",
            FindingSeverity::Critical,
            Some("container-analysis"),
        ),
    ];
    let aggregated = aggregate_hook_findings(findings);

    assert_eq!(aggregated.len(), 1);
    assert_eq!(aggregated[0].primary.hook_id, "docker-cgroup");
    assert_eq!(aggregated[0].related[0].hook_id, "docker-health");
    assert_eq!(
        aggregated[0].recommended_skill.as_deref(),
        Some("container-analysis")
    );
    assert_eq!(aggregated[0].topic, "external");
}

#[test]
fn aggregation_keeps_external_findings_without_skill_separate() {
    let findings = vec![
        external_finding("docker-health", FindingSeverity::Warning, None),
        external_finding("docker-cgroup", FindingSeverity::Warning, None),
    ];
    let aggregated = aggregate_hook_findings(findings);

    assert_eq!(aggregated.len(), 2);
}

#[test]
fn builtin_memory_aggregate_preserves_all_producer_registration_ids() {
    let findings = vec![
        EvaluatedHookFinding::builtin_with_facts(
            "high-memory-process",
            finding("high-memory-process", FindingSeverity::Info),
            Some(BuiltinFindingFacts::HighMemoryProcesses(
                HighMemoryProcessFacts {
                    confidence: MetricsConfidence::High,
                    processes: vec![ProcessMemoryFact {
                        pid: "1234".to_string(),
                        command_basename: "java".to_string(),
                        mem_pct: 20.0,
                        rss_kib: None,
                    }],
                },
            )),
        ),
        EvaluatedHookFinding::builtin_with_facts(
            "memory-pressure",
            finding("memory-pressure", FindingSeverity::Warning),
            Some(BuiltinFindingFacts::MemoryPressure(MemoryPressureFacts {
                confidence: MetricsConfidence::High,
                available_ratio: 0.08,
                swap_ratio: None,
            })),
        ),
    ];

    let aggregated = aggregate_hook_findings(findings);

    assert_eq!(aggregated.len(), 1);
    assert_eq!(
        aggregated[0].provenance,
        HookProvenance::Builtin {
            producer_registration_ids: BTreeSet::from([
                "high-memory-process".to_string(),
                "memory-pressure".to_string(),
            ]),
        }
    );
    assert_eq!(aggregated[0].builtin_facts.len(), 2);
}

#[test]
fn aggregation_does_not_cross_builtin_and_external_owner_classes() {
    let findings = vec![
        EvaluatedHookFinding::builtin(
            "memory-pressure",
            finding("memory-pressure", FindingSeverity::Warning),
        ),
        EvaluatedHookFinding::external(
            "external:0",
            finding("memory-pressure", FindingSeverity::Warning),
        ),
    ];

    let aggregated = aggregate_hook_findings(findings);

    assert_eq!(aggregated.len(), 2);
}

#[test]
fn aggregation_does_not_cross_external_registrations() {
    let findings = vec![
        EvaluatedHookFinding::external(
            "external:0",
            external_finding(
                "docker-health",
                FindingSeverity::Warning,
                Some("container-analysis"),
            ),
        ),
        EvaluatedHookFinding::external(
            "external:1",
            external_finding(
                "docker-cgroup",
                FindingSeverity::Critical,
                Some("container-analysis"),
            ),
        ),
    ];

    let aggregated = aggregate_hook_findings(findings);

    assert_eq!(aggregated.len(), 2);
}
