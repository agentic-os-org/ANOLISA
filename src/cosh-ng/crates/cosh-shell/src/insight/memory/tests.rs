use super::*;
use crate::insight::correlation::{InsightCorrelationState, MemoryPressureFact};
use crate::insight::model::{ExecutionScope, InsightConfidence, InsightSeverity, InsightSource};
use crate::types::{
    BuiltinFactRecord, BuiltinFindingFacts, CommandBlock, CommandStatus, FindingSeverity,
    HighMemoryProcessFacts, HookFinding, HookProvenance, MemoryPressureFacts, MetricsConfidence,
    OutputRefs, ProcessMemoryFact,
};

fn block(command: &str, ended_at_ms: u64) -> CommandBlock {
    CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "session-1".to_string(),
        command: command.to_string(),
        origin: Default::default(),
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: ended_at_ms.saturating_sub(10),
        ended_at_ms,
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

fn finding(hook_id: &str, severity: FindingSeverity, title: &str) -> HookFinding {
    HookFinding {
        hook_id: hook_id.to_string(),
        severity,
        title: title.to_string(),
        description: "structured builtin memory finding".to_string(),
        suggestion: "diagnose memory".to_string(),
        skill: None,
        cli_hint: None,
        context_refs: Vec::new(),
    }
}

fn builtin(registration_id: &str) -> HookProvenance {
    HookProvenance::Builtin {
        producer_registration_ids: BTreeSet::from([registration_id.to_string()]),
    }
}

fn builtin_pair() -> HookProvenance {
    HookProvenance::Builtin {
        producer_registration_ids: BTreeSet::from([
            "high-memory-process".to_string(),
            "memory-pressure".to_string(),
        ]),
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

fn process_record(command_basename: &str, mem_pct: f64) -> BuiltinFactRecord {
    BuiltinFactRecord {
        producer_registration_id: "high-memory-process".to_string(),
        facts: BuiltinFindingFacts::HighMemoryProcesses(HighMemoryProcessFacts {
            confidence: MetricsConfidence::High,
            processes: vec![ProcessMemoryFact {
                pid: "1234".to_string(),
                command_basename: command_basename.to_string(),
                mem_pct,
                rss_kib: Some(1024),
            }],
        }),
    }
}

fn adapt_with_facts(
    block: &CommandBlock,
    provenance: &HookProvenance,
    primary: &HookFinding,
    related: &[HookFinding],
    correlation: &mut InsightCorrelationState,
) -> MemoryInsightOutcome {
    let facts = std::iter::once(primary)
        .chain(related.iter())
        .filter_map(|finding| match finding.hook_id.as_str() {
            "memory-pressure" => Some(pressure_record(
                MetricsConfidence::High,
                match finding.severity {
                    FindingSeverity::Critical => 0.05,
                    FindingSeverity::Warning => 0.08,
                    FindingSeverity::Info => 0.5,
                },
            )),
            "high-memory-process" => Some(process_record(
                "java",
                match finding.severity {
                    FindingSeverity::Critical => 50.0,
                    FindingSeverity::Warning => 30.0,
                    FindingSeverity::Info => 20.0,
                },
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    adapt_memory_aggregate(
        block,
        MemoryAggregateView::new_with_facts(provenance, primary, related, &facts),
        correlation,
    )
}

#[test]
fn only_registered_builtin_memory_producers_are_claimed() {
    let block = block("free -m", 1_000);
    let mut correlation = InsightCorrelationState::default();
    let builtin_provenance = builtin("memory-pressure");
    let builtin_finding = finding(
        "memory-pressure",
        FindingSeverity::Warning,
        "Available memory is low: 100 MiB / 1000 MiB",
    );
    assert!(matches!(
        adapt_with_facts(
            &block,
            &builtin_provenance,
            &builtin_finding,
            &[],
            &mut correlation
        ),
        MemoryInsightOutcome::Claimed(Some(_))
    ));

    let external_provenance = HookProvenance::External {
        registration_key: "external:0".to_string(),
    };
    assert!(matches!(
        adapt_memory_aggregate(
            &block,
            MemoryAggregateView::new(&external_provenance, &builtin_finding, &[]),
            &mut correlation
        ),
        MemoryInsightOutcome::NotClaimed
    ));
}

#[test]
fn claimed_invalid_builtin_payload_is_an_error_not_legacy_fallback() {
    let provenance = builtin("memory-pressure");
    let corrupt = finding("corrupt-payload", FindingSeverity::Warning, "invalid");

    assert!(matches!(
        adapt_memory_aggregate(
            &block("free -m", 1_000),
            MemoryAggregateView::new(&provenance, &corrupt, &[]),
            &mut InsightCorrelationState::default()
        ),
        MemoryInsightOutcome::ClaimedError(_)
    ));
}

#[test]
fn process_presentation_title_is_not_policy_input() {
    let provenance = builtin("high-memory-process");
    let malformed = finding(
        "high-memory-process",
        FindingSeverity::Warning,
        "java worker uses high memory",
    );

    let facts = vec![process_record("java", 30.0)];
    let outcome = adapt_memory_aggregate(
        &block("ps aux", 1_000),
        MemoryAggregateView::new_with_facts(&provenance, &malformed, &[], &facts),
        &mut InsightCorrelationState::default(),
    );
    assert!(matches!(outcome, MemoryInsightOutcome::Claimed(Some(_))));
}

#[test]
fn candidate_only_process_requires_recent_same_scope_pressure() {
    let provenance = builtin("high-memory-process");
    let process = finding(
        "high-memory-process",
        FindingSeverity::Info,
        "java (PID 1234) uses 20.0% MEM",
    );
    let block = block("ps aux", 2_000);
    let mut correlation = InsightCorrelationState::default();
    assert!(matches!(
        adapt_with_facts(&block, &provenance, &process, &[], &mut correlation),
        MemoryInsightOutcome::Claimed(None)
    ));

    correlation.record(MemoryPressureFact {
        scope: ExecutionScope::local("session-1"),
        ended_at_ms: 1_000,
        severity: InsightSeverity::Warning,
        confidence: InsightConfidence::High,
        source_command_block_id: "cmd-pressure".to_string(),
        provider_safe_fact: "memory_pressure severity=Warning ended_at_ms=1000".to_string(),
    });
    let MemoryInsightOutcome::Claimed(Some(candidate)) =
        adapt_with_facts(&block, &provenance, &process, &[], &mut correlation)
    else {
        panic!("recent pressure should promote candidate-only process");
    };
    assert_eq!(candidate.source, InsightSource::Ps);
    assert_eq!(candidate.severity, InsightSeverity::Candidate);
}

#[test]
fn unknown_wrapper_is_claimed_but_never_correlated_or_shown() {
    let provenance = builtin("memory-pressure");
    let pressure = finding(
        "memory-pressure",
        FindingSeverity::Critical,
        "Available memory is low: 50 MiB / 1000 MiB",
    );

    assert!(matches!(
        adapt_with_facts(
            &block("ssh host free -m", 1_000),
            &provenance,
            &pressure,
            &[],
            &mut InsightCorrelationState::default()
        ),
        MemoryInsightOutcome::Claimed(None)
    ));
}

#[test]
fn direct_env_and_sudo_pressure_promote_following_process_candidate() {
    let pressure_provenance = builtin("memory-pressure");
    let process_provenance = builtin("high-memory-process");
    let pressure = finding(
        "memory-pressure",
        FindingSeverity::Warning,
        "Available memory is low: 100 MiB / 1000 MiB",
    );
    let process = finding(
        "high-memory-process",
        FindingSeverity::Info,
        "java (PID 1234) uses 20.0% MEM",
    );

    for command in [
        "free -m",
        "env LANG=C free -m",
        "sudo free -m",
        "sudo -u root free -m",
    ] {
        let mut correlation = InsightCorrelationState::default();
        assert!(matches!(
            adapt_with_facts(
                &block(command, 1_000),
                &pressure_provenance,
                &pressure,
                &[],
                &mut correlation
            ),
            MemoryInsightOutcome::Claimed(Some(_))
        ));
        assert!(matches!(
            adapt_with_facts(
                &block("ps aux", 1_001),
                &process_provenance,
                &process,
                &[],
                &mut correlation
            ),
            MemoryInsightOutcome::Claimed(Some(_))
        ));
    }
}

#[test]
fn recorded_pressure_fact_contains_only_minimal_provider_safe_fields() {
    let provenance = builtin("memory-pressure");
    let pressure = finding(
        "memory-pressure",
        FindingSeverity::Warning,
        "secret-process (PID 4321) under /private/cwd is high",
    );
    let mut unsafe_block = block("free -m", 1_000);
    unsafe_block.id = "cmd unsafe\nvalue".to_string();
    let mut correlation = InsightCorrelationState::default();

    let _ = adapt_with_facts(&unsafe_block, &provenance, &pressure, &[], &mut correlation);

    assert_eq!(
        correlation.recent_memory_pressure_facts(
            &ExecutionScope::local("session-1"),
            unsafe_block.ended_at_ms,
            "other-command",
        ),
        vec![
            "source_command_block_id=cmd_unsafe_value; memory_pressure severity=Warning ended_at_ms=1000"
                .to_string()
        ]
    );
}

#[test]
fn combined_top_pressure_and_process_produces_one_root_cause_candidate() {
    let provenance = builtin_pair();
    let pressure = finding(
        "memory-pressure",
        FindingSeverity::Critical,
        "Available memory is low: 50 MiB / 1000 MiB",
    );
    let process = finding(
        "high-memory-process",
        FindingSeverity::Warning,
        "java (PID 1234) uses 30.0% MEM",
    );

    let MemoryInsightOutcome::Claimed(Some(candidate)) = adapt_with_facts(
        &block("top -b -n1", 1_000),
        &provenance,
        &pressure,
        &[process],
        &mut InsightCorrelationState::default(),
    ) else {
        panic!("combined top finding should produce one candidate");
    };

    assert_eq!(candidate.source, InsightSource::Top);
    assert_eq!(candidate.severity, InsightSeverity::Critical);
    assert_eq!(
        candidate.topic,
        super::super::model::SuppressionTopic::MemoryRootCause
    );
    assert_eq!(candidate.entity, EntityKey::Process("java".to_string()));
}
