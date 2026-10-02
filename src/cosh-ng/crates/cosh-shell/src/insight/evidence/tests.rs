use super::{
    bounded_excerpt, build_evidence_bundle, build_evidence_bundle_with_budget,
    build_provider_evidence_payload, provider_target_facts, scenario_policy,
    take_bound_insight_metadata, BoundInsightMetadata, BundleBudget, EvidenceBundleInput,
    EvidenceScenario, ExcerptDirection, ProviderTargetFacts, BUILD_TEST_SIDE_BYTES,
    EXCERPT_MAX_BYTES, PROVIDER_CONTEXT_MAX_BYTES, RELATED_FACTS_MAX_BYTES, TARGET_FACTS_MAX_BYTES,
};
use crate::types::{CommandBlock, CommandStatus, OutputRefs};

#[test]
fn bound_metadata_normalizes_status_and_deduplicates_structured_facts() {
    let mut hints = vec![
        "__cosh_insight_evidence_status=Available".to_string(),
        "__cosh_insight_evidence=failure_class=PermissionDenied".to_string(),
        "__cosh_insight_evidence=failure_reason_0=ExitCode(126)".to_string(),
    ];

    let metadata = take_bound_insight_metadata(
        &mut hints,
        "Warning",
        "High",
        vec![
            "failure_class=PermissionDenied".to_string(),
            "failure_reason_0=ExitCode(126)".to_string(),
        ],
    );

    assert_eq!(metadata.evidence_status.as_deref(), Some("available"));
    assert_eq!(
        metadata.evidence,
        vec![
            "failure_class=PermissionDenied".to_string(),
            "failure_reason_0=ExitCode(126)".to_string(),
        ]
    );
    assert!(hints.is_empty());
}

#[test]
fn build_test_excerpt_keeps_head_diagnostic_and_tail_summary() {
    let mut lines = vec!["error: first diagnostic".to_string()];
    lines.extend((0..400).map(|index| format!("compile detail {index}: {}", "x".repeat(64))));
    lines.push("test result: FAILED. 1 failed".to_string());

    let excerpt = bounded_excerpt(&lines.join("\n"), EvidenceScenario::BuildOrTest);

    assert!(excerpt.text.starts_with("error: first diagnostic"));
    assert!(excerpt.text.ends_with("test result: FAILED. 1 failed"));
    assert!(excerpt.truncated);
    assert!(excerpt.text.len() <= EXCERPT_MAX_BYTES);
}

#[test]
fn build_test_excerpt_does_not_duplicate_overlapping_lines() {
    let output = "compile start\nwarning: example\ntest result: ok";

    let excerpt = bounded_excerpt(output, EvidenceScenario::BuildOrTest);

    assert_eq!(excerpt.text, output);
    assert!(!excerpt.truncated);
    assert_eq!(excerpt.text.matches("warning: example").count(), 1);
}

#[test]
fn runtime_exception_has_independent_head_tail_policy() {
    let output = format!(
        "Traceback (most recent call last):\n  File \"app.py\", line 1\n{}ValueError: boom\n",
        "runtime detail\n".repeat(300)
    );

    let excerpt = bounded_excerpt(&output, EvidenceScenario::RuntimeException);
    let policy = scenario_policy(EvidenceScenario::RuntimeException);

    assert_eq!(policy.direction, ExcerptDirection::HeadTail);
    assert_eq!(policy.max_lines, 120);
    assert_eq!(policy.max_bytes, EXCERPT_MAX_BYTES);
    assert!(excerpt.text.starts_with("Traceback"));
    assert!(excerpt.text.ends_with("ValueError: boom"));
}

#[test]
fn build_test_excerpt_respects_each_side_and_total_utf8_byte_caps() {
    let head = format!("HEAD诊断\n{}", "界".repeat(BUILD_TEST_SIDE_BYTES));
    let tail = format!("{}\nTAIL总结", "尾".repeat(BUILD_TEST_SIDE_BYTES));
    let excerpt = bounded_excerpt(
        &format!("{head}\n{}\n{tail}", "middle\n".repeat(500)),
        EvidenceScenario::BuildOrTest,
    );

    assert!(excerpt.text.contains("HEAD诊断"));
    assert!(excerpt.text.contains("TAIL总结"));
    assert!(excerpt.text.len() <= EXCERPT_MAX_BYTES);
    assert!(excerpt.text.is_char_boundary(excerpt.text.len()));
}

#[test]
fn failure_and_memory_scenarios_have_fixed_directions_and_budgets() {
    let classifier = scenario_policy(EvidenceScenario::FailureClassifier);
    assert_eq!(classifier.direction, ExcerptDirection::Head);
    assert_eq!(classifier.max_lines, 120);
    assert_eq!(classifier.max_bytes, 8 * 1024);

    for scenario in [
        EvidenceScenario::CommandNotFound,
        EvidenceScenario::PermissionDenied,
        EvidenceScenario::AbnormalSignal,
    ] {
        let policy = scenario_policy(scenario);
        assert_eq!(policy.direction, ExcerptDirection::Tail);
        assert_eq!(policy.max_lines, 120);
        assert_eq!(policy.max_bytes, EXCERPT_MAX_BYTES);
    }

    for scenario in [
        EvidenceScenario::FreeMemory,
        EvidenceScenario::TopProcesses,
        EvidenceScenario::PsProcesses,
    ] {
        let policy = scenario_policy(scenario);
        assert_eq!(policy.direction, ExcerptDirection::Head);
        assert_eq!(policy.max_lines, 120);
        assert_eq!(policy.max_bytes, EXCERPT_MAX_BYTES);
    }
}

#[test]
fn bundle_caps_each_section_and_removes_oldest_related_facts_first() {
    let bundle = build_evidence_bundle(EvidenceBundleInput {
        target_facts: ProviderTargetFacts::plain(format!(
            "identity=cmd-1\n{}",
            "f".repeat(6 * 1024)
        )),
        target_excerpt: format!("diagnostic\n{}", "e".repeat(16 * 1024)),
        related_facts: vec![
            format!("oldest\n{}", "a".repeat(4 * 1024)),
            format!("middle\n{}", "b".repeat(4 * 1024)),
            format!("newest\n{}", "c".repeat(4 * 1024)),
        ],
    });

    assert!(bundle.target_facts.len() <= TARGET_FACTS_MAX_BYTES);
    assert!(bundle.target_excerpt.len() <= EXCERPT_MAX_BYTES);
    assert!(bundle.related_facts.iter().map(String::len).sum::<usize>() <= RELATED_FACTS_MAX_BYTES);
    assert!(bundle.serialized_bytes <= PROVIDER_CONTEXT_MAX_BYTES);
    assert_eq!(bundle.removed_related_facts, 2);
    assert_eq!(bundle.related_facts.len(), 1);
    assert!(bundle.related_facts[0].starts_with("newest"));
}

#[test]
fn bundle_keeps_at_most_three_most_recent_related_facts() {
    let bundle = build_evidence_bundle(EvidenceBundleInput {
        target_facts: ProviderTargetFacts::plain("target"),
        target_excerpt: "excerpt".to_string(),
        related_facts: vec![
            "oldest".to_string(),
            "older".to_string(),
            "newer".to_string(),
            "newest".to_string(),
        ],
    });

    assert_eq!(bundle.related_facts, ["older", "newer", "newest"]);
    assert_eq!(bundle.removed_related_facts, 1);
}

#[test]
fn aggregate_budget_truncates_related_before_target_excerpt() {
    let bundle = build_evidence_bundle_with_budget(
        EvidenceBundleInput {
            target_facts: ProviderTargetFacts::plain("required-target-facts"),
            target_excerpt: "target-excerpt-content".to_string(),
            related_facts: vec!["old-related".to_string(), "new-related-content".to_string()],
        },
        BundleBudget {
            target_facts_bytes: 64,
            target_excerpt_bytes: 64,
            related_facts_bytes: 64,
            total_bytes: 48,
        },
    );

    assert_eq!(bundle.target_facts, "required-target-facts");
    assert_eq!(bundle.target_excerpt, "target-excerpt-content");
    assert!(bundle.removed_related_facts > 0 || bundle.related_truncated);
    assert!(!bundle.target_excerpt_truncated);
    assert!(bundle.serialized_bytes <= 48);
}

#[test]
fn aggregate_budget_truncates_target_excerpt_only_after_related_is_gone() {
    let bundle = build_evidence_bundle_with_budget(
        EvidenceBundleInput {
            target_facts: ProviderTargetFacts::plain("required-target-facts"),
            target_excerpt: "target-excerpt-content".to_string(),
            related_facts: vec!["old-related".to_string()],
        },
        BundleBudget {
            target_facts_bytes: 64,
            target_excerpt_bytes: 64,
            related_facts_bytes: 64,
            total_bytes: 25,
        },
    );

    assert_eq!(bundle.target_facts, "required-target-facts");
    assert!(bundle.related_facts.is_empty());
    assert!(bundle.target_excerpt_truncated);
    assert!(bundle.serialized_bytes <= 25);
}

#[test]
fn aggregate_budget_updates_facts_when_it_truncates_target_excerpt() {
    let bundle = build_evidence_bundle_with_budget(
        EvidenceBundleInput {
            target_facts: ProviderTargetFacts::with_status_parts(
                "command_id=cmd-1; truncation_status=",
                "; command=printf truncation_status=complete",
            ),
            target_excerpt: "excerpt-content-that-will-not-fit".to_string(),
            related_facts: Vec::new(),
        },
        BundleBudget {
            target_facts_bytes: 128,
            target_excerpt_bytes: 128,
            related_facts_bytes: 0,
            total_bytes: 100,
        },
    );

    assert!(bundle.target_excerpt_truncated);
    assert!(bundle.target_facts.contains("truncation_status=truncated"));
    assert!(bundle
        .target_facts
        .contains("command=printf truncation_status=complete"));
    assert!(bundle.serialized_bytes <= 100);
}

#[test]
fn serialized_provider_payload_closes_the_total_budget() {
    let other_context = 173;
    let payload = build_provider_evidence_payload(
        EvidenceBundleInput {
            target_facts: ProviderTargetFacts::plain(format!(
                "command_id=target; exit_code=2; execution_scope=LocalHost; evidence_status=Available; redaction_status=preview_redacted; severity=Critical; confidence=High; structured_evidence=failure_class=BuildOrTestFailure; {}",
                "command=x".repeat(2_000)
            )),
            target_excerpt: "excerpt".repeat(8_000),
            related_facts: (0..8)
                .map(|index| format!("related-{index}:{}", "x".repeat(4_000)))
                .collect(),
        },
        other_context,
    );

    assert!(payload.len() + other_context <= PROVIDER_CONTEXT_MAX_BYTES);
    for required in [
        "command_id=target",
        "exit_code=2",
        "execution_scope=LocalHost",
        "evidence_status=Available",
        "redaction_status=preview_redacted",
        "severity=Critical",
        "confidence=High",
        "structured_evidence=failure_class=BuildOrTestFailure",
        "bundle_status: target_excerpt_truncated=true",
    ] {
        assert!(payload.contains(required), "missing {required}: {payload}");
    }
}

#[test]
fn provider_target_facts_preserve_mandatory_identity_with_long_command() {
    let block = CommandBlock {
        id: "target-command-id".to_string(),
        session_id: "session-1".to_string(),
        command: "x".repeat(8 * 1024),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 1,
        ended_at_ms: 2,
        duration_ms: 1,
        exit_code: 2,
        status: CommandStatus::Failed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let facts = provider_target_facts(
        &block,
        "LocalHost",
        "UserInteractive",
        "available",
        "excerpt_included",
        "complete",
        &BoundInsightMetadata {
            evidence_status: Some("available".to_string()),
            severity: "Warning".to_string(),
            confidence: "High".to_string(),
            evidence: vec!["failure_class=BuildOrTestFailure".to_string()],
        },
    );

    assert!(facts.len() <= TARGET_FACTS_MAX_BYTES, "{}", facts.len());
    for required in [
        "command_id=target-command-id",
        "exit_code=2",
        "execution_scope=LocalHost",
        "evidence_status=available",
        "redaction_status=excerpt_included",
        "truncation_status=complete",
        "command=",
    ] {
        assert!(facts.contains(required), "missing {required}: {facts}");
    }
}
