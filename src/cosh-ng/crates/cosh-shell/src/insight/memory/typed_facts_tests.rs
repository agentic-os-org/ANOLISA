use super::*;
use crate::insight::correlation::InsightCorrelationState;
use crate::insight::model::{EntityKey, InsightConfidence, InsightSeverity, OutputExcerptStatus};
use crate::types::{
    BuiltinFactRecord, BuiltinFindingFacts, CommandBlock, CommandStatus, FindingSeverity,
    HighMemoryProcessFacts, HookFinding, HookProvenance, MemoryPressureFacts, MetricsConfidence,
    OutputRefs, ProcessMemoryFact,
};

fn block(command: &str) -> CommandBlock {
    CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "session-1".to_string(),
        command: command.to_string(),
        origin: Default::default(),
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 990,
        ended_at_ms: 1_000,
        duration_ms: 10,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: Some("terminal-output://session-1/cmd-1".to_string()),
            terminal_output_bytes: 100,
        },
        shell_environment_generation: None,
        audit_identity: None,
    }
}

fn finding(hook_id: &str, severity: FindingSeverity) -> HookFinding {
    HookFinding {
        hook_id: hook_id.to_string(),
        severity,
        title: "changed presentation".to_string(),
        description: "changed presentation".to_string(),
        suggestion: "diagnose memory".to_string(),
        skill: None,
        cli_hint: None,
        context_refs: Vec::new(),
    }
}

fn builtin(ids: &[&str]) -> HookProvenance {
    HookProvenance::Builtin {
        producer_registration_ids: ids.iter().map(|id| (*id).to_string()).collect(),
    }
}

fn pressure_record(confidence: MetricsConfidence, available_ratio: f64) -> BuiltinFactRecord {
    BuiltinFactRecord {
        producer_registration_id: "memory-pressure".to_string(),
        facts: BuiltinFindingFacts::MemoryPressure(MemoryPressureFacts {
            confidence,
            available_ratio,
            swap_ratio: Some(0.4),
        }),
    }
}

fn process_record(
    confidence: MetricsConfidence,
    command_basename: &str,
    mem_pct: f64,
) -> BuiltinFactRecord {
    BuiltinFactRecord {
        producer_registration_id: "high-memory-process".to_string(),
        facts: BuiltinFindingFacts::HighMemoryProcesses(HighMemoryProcessFacts {
            confidence,
            processes: vec![ProcessMemoryFact {
                pid: "1234".to_string(),
                command_basename: command_basename.to_string(),
                mem_pct,
                rss_kib: Some(1024),
            }],
        }),
    }
}

#[test]
fn claimed_builtin_memory_without_typed_facts_is_an_error() {
    let provenance = builtin(&["memory-pressure"]);
    let pressure = finding("memory-pressure", FindingSeverity::Warning);

    assert!(matches!(
        adapt_memory_aggregate(
            &block("free -m"),
            MemoryAggregateView::new(&provenance, &pressure, &[]),
            &mut InsightCorrelationState::default()
        ),
        MemoryInsightOutcome::ClaimedError("missing-memory-facts")
    ));
}

#[test]
fn typed_facts_not_presentation_text_drive_memory_policy() {
    let provenance = builtin(&["memory-pressure", "high-memory-process"]);
    let mut pressure = finding("memory-pressure", FindingSeverity::Info);
    pressure.description = "Confidence is lower and title carries no policy data".to_string();
    let process = finding("high-memory-process", FindingSeverity::Critical);
    let facts = vec![
        pressure_record(MetricsConfidence::High, 0.08),
        process_record(MetricsConfidence::High, "java", 25.0),
    ];

    let MemoryInsightOutcome::Claimed(Some(candidate)) = adapt_memory_aggregate(
        &block("top -b -n1"),
        MemoryAggregateView::new_with_facts(&provenance, &pressure, &[process], &facts),
        &mut InsightCorrelationState::default(),
    ) else {
        panic!("valid typed facts should produce a candidate");
    };

    assert_eq!(candidate.confidence, InsightConfidence::High);
    assert_eq!(candidate.severity, InsightSeverity::Warning);
    assert_eq!(candidate.entity, EntityKey::Process("java".to_string()));
}

#[test]
fn memory_target_preserves_capture_truncation_status() {
    let provenance = builtin(&["memory-pressure"]);
    let pressure = finding("memory-pressure", FindingSeverity::Warning);
    let facts = vec![pressure_record(MetricsConfidence::High, 0.08)];
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-memory-capture-status-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let output = dir.join("output.txt");
    std::fs::write(&output, "memory pressure").expect("output");
    let mut command = block("free -m");
    command.output.terminal_output_ref = Some(output.to_string_lossy().to_string());
    command.output.terminal_output_bytes = crate::types::COMMAND_OUTPUT_REF_MAX_BYTES as u64 + 1;

    let MemoryInsightOutcome::Claimed(Some(candidate)) = adapt_memory_aggregate(
        &command,
        MemoryAggregateView::new_with_facts(&provenance, &pressure, &[], &facts),
        &mut InsightCorrelationState::default(),
    ) else {
        panic!("valid pressure should produce candidate");
    };

    std::fs::remove_dir_all(dir).ok();
    let PromptSuggestion::AgentPrompt { binding } = candidate.suggestion.expect("suggestion")
    else {
        panic!("memory candidate should bind an Agent prompt");
    };
    assert_eq!(
        binding.target.evidence_status,
        OutputExcerptStatus::Truncated
    );
}

#[test]
fn memory_target_preserves_read_failed_status() {
    let provenance = builtin(&["memory-pressure"]);
    let pressure = finding("memory-pressure", FindingSeverity::Warning);
    let facts = vec![pressure_record(MetricsConfidence::High, 0.08)];
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-memory-read-failed-status-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let output = dir.join("output.txt");
    std::fs::write(&output, [0xff, 0xfe]).expect("output");
    let mut command = block("free -m");
    command.output.terminal_output_ref = Some(output.to_string_lossy().to_string());
    command.output.terminal_output_bytes = 2;

    let MemoryInsightOutcome::Claimed(Some(candidate)) = adapt_memory_aggregate(
        &command,
        MemoryAggregateView::new_with_facts(&provenance, &pressure, &[], &facts),
        &mut InsightCorrelationState::default(),
    ) else {
        panic!("valid pressure should produce candidate");
    };

    std::fs::remove_dir_all(dir).ok();
    let PromptSuggestion::AgentPrompt { binding } = candidate.suggestion.expect("suggestion")
    else {
        panic!("memory candidate should bind an Agent prompt");
    };
    assert_eq!(
        binding.target.evidence_status,
        OutputExcerptStatus::ReadFailed
    );
}

#[test]
fn invalid_typed_facts_are_claimed_errors() {
    let provenance = builtin(&["memory-pressure"]);
    let pressure = finding("memory-pressure", FindingSeverity::Warning);

    for facts in [
        vec![pressure_record(MetricsConfidence::High, f64::NAN)],
        vec![pressure_record(MetricsConfidence::High, 1.1)],
        vec![process_record(MetricsConfidence::High, "java", 25.0)],
        vec![
            pressure_record(MetricsConfidence::High, 0.08),
            pressure_record(MetricsConfidence::High, 0.08),
        ],
        vec![
            pressure_record(MetricsConfidence::High, 0.08),
            pressure_record(MetricsConfidence::High, 0.05),
        ],
    ] {
        assert!(matches!(
            adapt_memory_aggregate(
                &block("free -m"),
                MemoryAggregateView::new_with_facts(&provenance, &pressure, &[], &facts),
                &mut InsightCorrelationState::default()
            ),
            MemoryInsightOutcome::ClaimedError(_)
        ));
    }

    let process_provenance = builtin(&["high-memory-process"]);
    let process = finding("high-memory-process", FindingSeverity::Warning);
    let unsafe_process = vec![process_record(MetricsConfidence::High, "java worker", 25.0)];
    assert!(matches!(
        adapt_memory_aggregate(
            &block("ps aux"),
            MemoryAggregateView::new_with_facts(
                &process_provenance,
                &process,
                &[],
                &unsafe_process,
            ),
            &mut InsightCorrelationState::default()
        ),
        MemoryInsightOutcome::ClaimedError(_)
    ));
}

#[test]
fn low_confidence_typed_memory_facts_are_claimed_silent() {
    let pressure_provenance = builtin(&["memory-pressure"]);
    let pressure = finding("memory-pressure", FindingSeverity::Critical);
    let pressure_facts = vec![pressure_record(MetricsConfidence::Low, 0.04)];
    assert!(matches!(
        adapt_memory_aggregate(
            &block("free -m"),
            MemoryAggregateView::new_with_facts(
                &pressure_provenance,
                &pressure,
                &[],
                &pressure_facts,
            ),
            &mut InsightCorrelationState::default()
        ),
        MemoryInsightOutcome::Claimed(None)
    ));

    let process_provenance = builtin(&["high-memory-process"]);
    let process = finding("high-memory-process", FindingSeverity::Critical);
    let process_facts = vec![process_record(MetricsConfidence::Low, "java", 50.0)];
    assert!(matches!(
        adapt_memory_aggregate(
            &block("ps aux"),
            MemoryAggregateView::new_with_facts(&process_provenance, &process, &[], &process_facts,),
            &mut InsightCorrelationState::default()
        ),
        MemoryInsightOutcome::Claimed(None)
    ));
}
