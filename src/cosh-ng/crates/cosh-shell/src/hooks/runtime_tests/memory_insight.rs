use super::*;
use crate::types::{
    BuiltinFindingFacts, EvaluatedHookFinding, HighMemoryProcessFacts, MemoryPressureFacts,
    MetricsConfidence, ProcessMemoryFact,
};

fn evaluated(
    registration_id: &str,
    finding: HookFinding,
) -> crate::hooks::aggregate::AggregatedHookFinding {
    let builtin_facts = match registration_id {
        "memory-pressure" => Some(BuiltinFindingFacts::MemoryPressure(MemoryPressureFacts {
            confidence: MetricsConfidence::High,
            available_ratio: match finding.severity {
                FindingSeverity::Critical => 0.05,
                FindingSeverity::Warning => 0.08,
                FindingSeverity::Info => 0.5,
            },
            swap_ratio: None,
        })),
        "high-memory-process" => Some(BuiltinFindingFacts::HighMemoryProcesses(
            HighMemoryProcessFacts {
                confidence: MetricsConfidence::High,
                processes: vec![ProcessMemoryFact {
                    pid: "1234".to_string(),
                    command_basename: "java".to_string(),
                    mem_pct: match finding.severity {
                        FindingSeverity::Critical => 50.0,
                        FindingSeverity::Warning => 30.0,
                        FindingSeverity::Info => 20.0,
                    },
                    rss_kib: None,
                }],
            },
        )),
        _ => None,
    };
    aggregate_hook_findings(vec![EvaluatedHookFinding::builtin_with_facts(
        registration_id,
        finding,
        builtin_facts,
    )])
    .remove(0)
}

#[test]
fn builtin_memory_candidate_uses_insight_owner_only() {
    let aggregate = evaluated(
        "memory-pressure",
        finding("memory-pressure", FindingSeverity::Critical),
    );
    let block = block_with_command("free -m");
    let mut state = InlineState::default();

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert!(state.pending_command_insight.is_some());
    assert!(state.hooks.findings.is_empty());
    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.pending_consultation_queue.is_empty());
}

#[test]
fn builtin_memory_no_candidate_still_stops_legacy_route() {
    let aggregate = evaluated(
        "memory-pressure",
        finding("memory-pressure", FindingSeverity::Info),
    );
    let block = block_with_command("free -m");
    let mut state = InlineState::default();

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert!(state.pending_command_insight.is_none());
    assert!(state.hooks.findings.is_empty());
    assert!(state.hooks.pending_consultation_queue.is_empty());
}

#[test]
fn builtin_memory_adapter_error_still_stops_legacy_route() {
    let aggregate = evaluated(
        "memory-pressure",
        finding("corrupt-payload", FindingSeverity::Warning),
    );
    let block = block_with_command("free -m");
    let mut state = InlineState::default();

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert!(state.pending_command_insight.is_none());
    assert!(state.hooks.findings.is_empty());
    assert!(state.hooks.pending_consultation_queue.is_empty());
}

#[test]
fn same_name_external_memory_finding_keeps_legacy_owner() {
    let aggregate = aggregate_hook_findings(vec![EvaluatedHookFinding::external(
        "external:0",
        finding("memory-pressure", FindingSeverity::Warning),
    )])
    .remove(0);
    let block = block_with_command("free -m");
    let mut state = InlineState::default();

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert!(state.pending_command_insight.is_none());
    assert_eq!(state.hooks.findings.len(), 1);
}

#[test]
fn manual_mode_claims_builtin_memory_without_pending_surface() {
    let aggregate = evaluated(
        "memory-pressure",
        finding("memory-pressure", FindingSeverity::Critical),
    );
    let block = block_with_command("free -m");
    let mut state = InlineState {
        analysis_mode: AnalysisMode::Manual,
        ..InlineState::default()
    };

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert!(state.pending_command_insight.is_none());
    assert!(state.hooks.findings.is_empty());
}

#[test]
fn non_user_builtin_memory_does_not_enter_pending_or_correlation() {
    let pressure = evaluated(
        "memory-pressure",
        finding("memory-pressure", FindingSeverity::Critical),
    );
    let process = evaluated(
        "high-memory-process",
        finding("high-memory-process", FindingSeverity::Info),
    );
    let pressure_block = block_with_command_at("free -m", 1_000);
    let process_block = block_with_command_at("ps aux", 1_001);
    let mut state = InlineState::default();

    record_aggregated_hook_finding_with_origin(
        &pressure_block,
        pressure,
        CommandOrigin::ProviderTool,
        InterventionGates::eligible(),
        &mut state,
    );
    record_aggregated_hook_finding(&process_block, process, &mut state);

    assert!(state.pending_command_insight.is_none());
    assert!(state.hooks.findings.is_empty());
}
