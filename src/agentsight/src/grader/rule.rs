//! Deterministic rule-based conversation grader.

use super::input::EvaluationInput;
use super::types::{
    EvaluationDimension, EvaluationFinding, EvaluationMetadata, EvaluationRef, EvaluationResult,
    GraderType, RULE_GRADER_VERSION, RootCause, Verdict,
};
use crate::grader::evidence::{
    first_event_refs, genai_ref, has_usable_output, interruption_ref, looks_like_tool_failure,
};
use uuid::Uuid;

/// Deterministic MVP grader for conversation snapshots.
pub struct RuleGrader;

impl RuleGrader {
    /// Evaluate a conversation snapshot with the current deterministic rule set.
    pub fn evaluate(input: &EvaluationInput) -> EvaluationResult {
        let completion = score_completion(input);
        let runtime = score_runtime_health(input);
        let tool_use = score_tool_use(input);
        let efficiency = score_efficiency(input);
        let safety = score_safety(input);

        let dimensions = vec![
            completion.clone(),
            runtime.clone(),
            tool_use.clone(),
            efficiency.clone(),
            safety.clone(),
        ];
        let weighted_score = round_score(
            completion.score * 0.35
                + runtime.score * 0.25
                + tool_use.score * 0.20
                + efficiency.score * 0.10
                + safety.score * 0.10,
        );
        let findings = build_findings(input, &dimensions);
        let root_cause = select_root_cause(input, &dimensions, &findings);
        let verdict = select_verdict(input, weighted_score, root_cause, &dimensions, &findings);

        EvaluationResult {
            target_type: input.target_type,
            target_id: input.target_id.clone(),
            run_id: Uuid::new_v4().to_string(),
            input_hash: input.input_hash.clone(),
            verdict,
            score: weighted_score,
            summary: summary_for(verdict, root_cause),
            root_cause,
            recommended_action: recommended_action_for(verdict, root_cause).to_string(),
            dimensions,
            findings,
            metadata: EvaluationMetadata {
                evaluated_with_pending: input.evaluated_with_pending,
                pending_call_count: input.pending_call_count,
                input_event_count: input.events.len(),
                grader_type: GraderType::Rule,
                grader_version: RULE_GRADER_VERSION.to_string(),
                rubric_version: None,
                judge_model: None,
                prompt_hash: None,
                confidence: None,
            },
        }
    }
}

fn score_completion(input: &EvaluationInput) -> EvaluationDimension {
    let output_refs: Vec<EvaluationRef> = input
        .events
        .iter()
        .filter(|event| has_usable_output(event))
        .map(|event| genai_ref(&input.target_id, event, "Assistant output"))
        .collect();

    if output_refs.is_empty() {
        return dimension(
            "completion",
            0.0,
            "No usable assistant output was captured.",
            first_event_refs(input, "No output"),
        );
    }

    let pending_penalty = if input.evaluated_with_pending {
        0.15
    } else {
        0.0
    };
    dimension(
        "completion",
        1.0 - pending_penalty,
        if input.evaluated_with_pending {
            "A usable output exists, but the snapshot still has pending calls."
        } else {
            "A usable assistant output was captured."
        },
        output_refs,
    )
}

fn score_runtime_health(input: &EvaluationInput) -> EvaluationDimension {
    let interrupted_refs: Vec<EvaluationRef> = input
        .events
        .iter()
        .filter(|event| event.status.as_deref() == Some("interrupted"))
        .map(|event| genai_ref(&input.target_id, event, "Interrupted LLM call"))
        .collect();
    if !interrupted_refs.is_empty() {
        return dimension(
            "runtime_health",
            0.0,
            "One or more LLM calls were interrupted.",
            interrupted_refs,
        );
    }

    let unresolved: Vec<EvaluationRef> = input
        .interruptions
        .iter()
        .filter(|record| !record.resolved)
        .map(|record| interruption_ref(&input.target_id, record))
        .collect();
    if !unresolved.is_empty() {
        return dimension(
            "runtime_health",
            0.45,
            "Unresolved interruption signals were captured for this conversation.",
            unresolved,
        );
    }

    if input.evaluated_with_pending {
        return dimension(
            "runtime_health",
            0.75,
            "The snapshot contains pending calls and may still change.",
            first_event_refs(input, "Pending call"),
        );
    }

    dimension(
        "runtime_health",
        1.0,
        "No runtime interruption was detected.",
        Vec::new(),
    )
}

fn score_tool_use(input: &EvaluationInput) -> EvaluationDimension {
    let failed_tool_refs: Vec<EvaluationRef> = input
        .events
        .iter()
        .filter(|event| looks_like_tool_failure(event))
        .map(|event| genai_ref(&input.target_id, event, "Tool failure signal"))
        .collect();
    if !failed_tool_refs.is_empty() {
        return dimension(
            "tool_use",
            0.45,
            "Tool output contains deterministic error signals.",
            failed_tool_refs,
        );
    }

    let call_count = input.events.len();
    if call_count > 12 {
        return dimension(
            "tool_use",
            0.55,
            "The conversation required an unusually large number of LLM calls.",
            first_event_refs(input, "Repeated calls"),
        );
    }

    dimension(
        "tool_use",
        1.0,
        "No deterministic tool failure was detected.",
        Vec::new(),
    )
}

fn score_efficiency(input: &EvaluationInput) -> EvaluationDimension {
    let total_tokens: i64 = input.events.iter().map(|event| event.total_tokens).sum();
    if total_tokens >= 200_000 || input.events.len() > 20 {
        return dimension(
            "efficiency",
            0.35,
            "Token usage or call count is unusually high for a single conversation.",
            first_event_refs(input, "High cost"),
        );
    }
    if total_tokens >= 64_000 || input.events.len() > 10 {
        return dimension(
            "efficiency",
            0.65,
            "Token usage or call count is elevated for a single conversation.",
            first_event_refs(input, "Elevated cost"),
        );
    }

    dimension(
        "efficiency",
        1.0,
        "Token usage and call count are within normal bounds.",
        Vec::new(),
    )
}

fn score_safety(input: &EvaluationInput) -> EvaluationDimension {
    let safety_refs: Vec<EvaluationRef> = input
        .interruptions
        .iter()
        .filter(|record| !record.resolved && record.interruption_type.contains("safety"))
        .map(|record| interruption_ref(&input.target_id, record))
        .collect();
    if !safety_refs.is_empty() {
        return dimension(
            "safety",
            0.0,
            "Safety-related interruption signal was captured.",
            safety_refs,
        );
    }

    dimension(
        "safety",
        1.0,
        "No safety-specific signal was available or triggered.",
        Vec::new(),
    )
}

fn build_findings(
    input: &EvaluationInput,
    dimensions: &[EvaluationDimension],
) -> Vec<EvaluationFinding> {
    let mut findings = Vec::new();

    if !dimensions
        .iter()
        .any(|dimension| dimension.name == "completion" && dimension.score > 0.0)
    {
        findings.push(finding(
            "no_final_answer",
            "critical",
            "The conversation has no usable assistant output.",
            first_event_refs(input, "No output"),
        ));
    }

    for event in input
        .events
        .iter()
        .filter(|event| event.status.as_deref() == Some("interrupted"))
    {
        findings.push(finding(
            "interrupted_main_call",
            "critical",
            "An LLM call was interrupted before normal completion.",
            vec![genai_ref(&input.target_id, event, "Interrupted call")],
        ));
    }

    if input.evaluated_with_pending {
        findings.push(finding(
            "partial_snapshot",
            "medium",
            "Evaluation was forced while LLM calls were still pending.",
            first_event_refs(input, "Pending snapshot"),
        ));
    }

    for record in input.interruptions.iter().filter(|record| !record.resolved) {
        findings.push(finding(
            &record.interruption_type,
            severity_to_finding(&record.severity),
            "An unresolved interruption was recorded for this conversation.",
            vec![interruption_ref(&input.target_id, record)],
        ));
    }

    if input.events.iter().any(looks_like_tool_failure) {
        findings.push(finding(
            "tool_failure",
            "medium",
            "Tool output contains an error-like signal.",
            input
                .events
                .iter()
                .filter(|event| looks_like_tool_failure(event))
                .map(|event| genai_ref(&input.target_id, event, "Tool failure"))
                .collect(),
        ));
    }

    if input.events.len() > 12 {
        findings.push(finding(
            "loop_detected",
            "medium",
            "The conversation used many LLM calls and may need loop inspection.",
            first_event_refs(input, "Repeated calls"),
        ));
    }

    findings
}

fn select_root_cause(
    input: &EvaluationInput,
    dimensions: &[EvaluationDimension],
    findings: &[EvaluationFinding],
) -> RootCause {
    let completion_failed = dimensions.iter().any(|dimension| {
        dimension.name == "completion" && (dimension.score - 0.0).abs() < f64::EPSILON
    });
    if completion_failed {
        return RootCause::NoFinalAnswer;
    }
    if input
        .events
        .iter()
        .any(|event| event.status.as_deref() == Some("interrupted"))
    {
        return RootCause::InterruptedMainCall;
    }
    if findings.iter().any(|finding| finding.code == "agent_crash") {
        return RootCause::AgentCrash;
    }
    if findings.iter().any(|finding| {
        matches!(
            finding.code.as_str(),
            "llm_error"
                | "sse_truncated"
                | "network_timeout"
                | "service_unavailable"
                | "rate_limit"
                | "auth_error"
                | "context_overflow"
                | "token_limit"
        )
    }) {
        return RootCause::RuntimeError;
    }
    if findings
        .iter()
        .any(|finding| finding.code == "tool_failure")
    {
        return RootCause::ToolFailure;
    }
    if findings
        .iter()
        .any(|finding| finding.code.contains("safety"))
    {
        return RootCause::SafetyRisk;
    }
    if findings.iter().any(|finding| {
        matches!(
            finding.code.as_str(),
            "loop_detected" | "retry_storm" | "dead_loop"
        )
    }) {
        return RootCause::LoopDetected;
    }
    if dimensions
        .iter()
        .any(|dimension| dimension.name == "efficiency" && dimension.score < 0.5)
    {
        return RootCause::ExcessiveCost;
    }
    if input.evaluated_with_pending {
        return RootCause::PartialSnapshot;
    }
    RootCause::None
}

fn select_verdict(
    input: &EvaluationInput,
    score: f64,
    root_cause: RootCause,
    dimensions: &[EvaluationDimension],
    findings: &[EvaluationFinding],
) -> Verdict {
    if matches!(
        root_cause,
        RootCause::NoFinalAnswer | RootCause::InterruptedMainCall
    ) {
        return Verdict::Fail;
    }
    if score < 0.5 {
        return Verdict::Fail;
    }
    let has_failed_dimension = dimensions
        .iter()
        .any(|dimension| dimension.verdict == Verdict::Fail);
    if input.evaluated_with_pending
        || score < 0.8
        || root_cause != RootCause::None
        || has_failed_dimension
        || findings.iter().any(|finding| finding.severity != "low")
    {
        return Verdict::Warn;
    }
    Verdict::Pass
}

fn dimension(
    name: &str,
    score: f64,
    reason: &str,
    evidence_refs: Vec<EvaluationRef>,
) -> EvaluationDimension {
    EvaluationDimension {
        name: name.to_string(),
        score: round_score(score),
        verdict: verdict_for_score(score),
        reason: reason.to_string(),
        evidence_refs,
    }
}

fn finding(
    code: &str,
    severity: &str,
    message: &str,
    evidence_refs: Vec<EvaluationRef>,
) -> EvaluationFinding {
    EvaluationFinding {
        code: code.to_string(),
        severity: severity.to_string(),
        message: message.to_string(),
        evidence_refs,
    }
}

fn verdict_for_score(score: f64) -> Verdict {
    if score >= 0.8 {
        Verdict::Pass
    } else if score >= 0.5 {
        Verdict::Warn
    } else {
        Verdict::Fail
    }
}

fn summary_for(verdict: Verdict, root_cause: RootCause) -> String {
    match verdict {
        Verdict::Pass => {
            "Conversation completed successfully with no deterministic quality issue.".to_string()
        }
        Verdict::Warn => format!(
            "Conversation is usable but needs review for {}.",
            root_cause.as_str()
        ),
        Verdict::Fail => format!(
            "Conversation failed quality evaluation because of {}.",
            root_cause.as_str()
        ),
    }
}

fn recommended_action_for(verdict: Verdict, root_cause: RootCause) -> &'static str {
    match (verdict, root_cause) {
        (Verdict::Pass, _) => "No immediate action required.",
        (_, RootCause::NoFinalAnswer) => {
            "Inspect the final LLM call and provider response parsing."
        }
        (_, RootCause::InterruptedMainCall) => {
            "Inspect interruption evidence and retry the conversation after fixing runtime stability."
        }
        (_, RootCause::AgentCrash) => "Inspect agent health and crash diagnostics before retrying.",
        (_, RootCause::RuntimeError) => {
            "Inspect provider errors, network stability, and retry behavior."
        }
        (_, RootCause::ToolFailure) => "Inspect failing tool calls and tool response parsing.",
        (_, RootCause::SafetyRisk) => {
            "Review safety/security findings before re-running the agent."
        }
        (_, RootCause::LoopDetected) => "Inspect repeated calls and tighten stopping conditions.",
        (_, RootCause::ExcessiveCost) => {
            "Review prompts, tool outputs, and token-saving opportunities."
        }
        (_, RootCause::PartialSnapshot) => {
            "Wait for pending calls to complete or keep the partial result marked as forced."
        }
        (_, RootCause::None) => "Review warnings and supporting evidence.",
    }
}

fn severity_to_finding(severity: &str) -> &'static str {
    match severity {
        "critical" | "high" => "high",
        "medium" => "medium",
        _ => "low",
    }
}

fn round_score(score: f64) -> f64 {
    let clamped = score.clamp(0.0, 1.0);
    (clamped * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grader::types::TargetType;
    use crate::storage::sqlite::InterruptionRecord;
    use crate::storage::sqlite::genai::TraceEventDetail;

    fn event(id: i64, status: &str, output_tokens: i64) -> TraceEventDetail {
        TraceEventDetail {
            id,
            call_id: Some(format!("call-{id}")),
            start_timestamp_ns: id * 100,
            end_timestamp_ns: Some(id * 100 + 50),
            model: Some("gpt-test".to_string()),
            input_tokens: 100,
            output_tokens,
            total_tokens: 100 + output_tokens,
            input_messages: None,
            output_messages: if output_tokens > 0 {
                Some(r#"[{"role":"assistant","content":"done"}]"#.to_string())
            } else {
                Some("[]".to_string())
            },
            system_instructions: None,
            agent_name: Some("agent".to_string()),
            process_name: None,
            pid: Some(1),
            user_query: Some("do work".to_string()),
            event_json: None,
            trace_id: Some(format!("trace-{id}")),
            conversation_id: Some("conv-1".to_string()),
            cache_read_tokens: None,
            status: Some(status.to_string()),
            interruption_type: None,
        }
    }

    fn input(events: Vec<TraceEventDetail>) -> EvaluationInput {
        EvaluationInput {
            target_type: TargetType::Conversation,
            target_id: "conv-1".to_string(),
            events,
            interruptions: Vec::new(),
            input_hash: "hash".to_string(),
            evaluated_with_pending: false,
            pending_call_count: 0,
        }
    }

    fn interruption(interruption_type: &str, severity: &str, resolved: bool) -> InterruptionRecord {
        InterruptionRecord {
            id: 1,
            interruption_id: format!("intr-{interruption_type}"),
            session_id: Some("session-1".to_string()),
            trace_id: Some("trace-1".to_string()),
            conversation_id: Some("conv-1".to_string()),
            call_id: Some("call-1".to_string()),
            pid: Some(1),
            agent_name: Some("agent".to_string()),
            interruption_type: interruption_type.to_string(),
            severity: severity.to_string(),
            occurred_at_ns: 1_700_000_000_000_000_000,
            detail: None,
            resolved,
        }
    }

    #[test]
    fn passes_completed_conversation_with_output() {
        let result = RuleGrader::evaluate(&input(vec![event(1, "complete", 10)]));

        assert_eq!(result.verdict, Verdict::Pass);
        assert_eq!(result.root_cause, RootCause::None);
        assert!(result.score >= 0.8);
    }

    #[test]
    fn weighted_score_and_score_cutoffs_are_discriminating() {
        let mut snapshot = input(vec![event(1, "complete", 250_000)]);
        snapshot.interruptions = vec![interruption("rate_limit", "medium", false)];
        snapshot.events[0].output_messages = Some(
            r#"[{"role":"assistant","content":"tool_call_response: {\"error\":\"failed\"}"}]"#
                .to_string(),
        );

        let result = RuleGrader::evaluate(&snapshot);

        assert_eq!(result.score, 0.69);
        assert_eq!(result.verdict, Verdict::Warn);
        assert_eq!(verdict_for_score(0.80), Verdict::Pass);
        assert_eq!(verdict_for_score(0.79), Verdict::Warn);
        assert_eq!(verdict_for_score(0.50), Verdict::Warn);
        assert_eq!(verdict_for_score(0.49), Verdict::Fail);
    }

    #[test]
    fn warns_when_dimension_fails_despite_high_weighted_score() {
        let result = RuleGrader::evaluate(&input(vec![event(1, "complete", 250_000)]));
        let efficiency = result
            .dimensions
            .iter()
            .find(|dimension| dimension.name == "efficiency")
            .expect("efficiency dimension should exist");

        assert_eq!(result.score, 0.94);
        assert_eq!(result.root_cause, RootCause::ExcessiveCost);
        assert_eq!(efficiency.verdict, Verdict::Fail);
        assert_eq!(result.verdict, Verdict::Warn);
        assert_ne!(
            result.summary,
            "Conversation completed successfully with no deterministic quality issue."
        );
        assert_ne!(result.recommended_action, "No immediate action required.");
    }

    #[test]
    fn fails_when_no_usable_output_exists() {
        let result = RuleGrader::evaluate(&input(vec![event(1, "complete", 0)]));

        assert_eq!(result.verdict, Verdict::Fail);
        assert_eq!(result.root_cause, RootCause::NoFinalAnswer);
    }

    #[test]
    fn ignores_event_json_metadata_when_no_output_exists() {
        let mut no_output = event(1, "complete", 0);
        no_output.output_messages = None;
        no_output.event_json = Some(
            r#"{"model":"gpt-test","user_query":"do work","response":{"messages":[]}}"#.to_string(),
        );

        let result = RuleGrader::evaluate(&input(vec![no_output]));

        assert_eq!(result.verdict, Verdict::Fail);
        assert_eq!(result.root_cause, RootCause::NoFinalAnswer);
    }

    #[test]
    fn ignores_role_only_output_messages() {
        let mut no_output = event(1, "complete", 0);
        no_output.output_messages = Some(r#"[{"role":"assistant"}]"#.to_string());

        let result = RuleGrader::evaluate(&input(vec![no_output]));

        assert_eq!(result.verdict, Verdict::Fail);
        assert_eq!(result.root_cause, RootCause::NoFinalAnswer);
    }

    #[test]
    fn forced_pending_snapshot_warns_without_hard_failure() {
        let mut snapshot = input(vec![event(1, "pending", 10)]);
        snapshot.evaluated_with_pending = true;
        snapshot.pending_call_count = 1;

        let result = RuleGrader::evaluate(&snapshot);

        assert_eq!(result.verdict, Verdict::Warn);
        assert_eq!(result.root_cause, RootCause::PartialSnapshot);
        assert!(result.metadata.evaluated_with_pending);
    }

    #[test]
    fn resolved_safety_interruption_does_not_penalize_safety() {
        let mut snapshot = input(vec![event(1, "complete", 10)]);
        snapshot.interruptions = vec![interruption("safety_filter", "medium", true)];

        let result = RuleGrader::evaluate(&snapshot);
        let safety = result
            .dimensions
            .iter()
            .find(|dimension| dimension.name == "safety")
            .expect("safety dimension should exist");

        assert_eq!(safety.score, 1.0);
        assert_eq!(result.root_cause, RootCause::None);
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.code == "safety_filter")
        );
    }

    #[test]
    fn unresolved_interruption_codes_select_specific_root_causes() {
        for code in [
            "rate_limit",
            "auth_error",
            "context_overflow",
            "token_limit",
        ] {
            let mut snapshot = input(vec![event(1, "complete", 10)]);
            snapshot.interruptions = vec![interruption(code, "high", false)];

            let result = RuleGrader::evaluate(&snapshot);

            assert_eq!(result.root_cause, RootCause::RuntimeError, "{code}");
            assert!(!result.summary.ends_with("none."));
        }

        for code in ["retry_storm", "dead_loop"] {
            let mut snapshot = input(vec![event(1, "complete", 10)]);
            snapshot.interruptions = vec![interruption(code, "critical", false)];

            let result = RuleGrader::evaluate(&snapshot);

            assert_eq!(result.root_cause, RootCause::LoopDetected, "{code}");
            assert!(!result.summary.ends_with("none."));
        }
    }
}
