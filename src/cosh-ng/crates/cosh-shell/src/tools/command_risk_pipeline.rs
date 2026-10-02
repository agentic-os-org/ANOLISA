//! Pipeline-shape assessment for the command-risk classifier: aggregates
//! per-stage verdicts, tracks downloaded-code execution, and applies the
//! diagnostic and readonly pipeline heuristics.

use super::command_risk::{
    stage_assessment, AssessmentConfidence, AssessmentPolicy, AutoAllowEvidence, CommandAssessment,
    CommandShape, ExecutionDecision, InteractionRequirement, OutputExposure, OutputStability,
    RiskImpact, SideEffectClass,
};
use super::command_risk_build::{
    assessment, basename, dedupe_reasons, downloaded_program_file,
    interpreter_consumes_stdin_as_program, max_output_exposure, max_output_stability,
    min_confidence, network_download_effect,
};
use super::command_risk_launcher::{walk_launcher_chain, LauncherWalk};
use super::command_risk_parser::{is_env_assignment, ParsedCommand};
use super::command_risk_verdict::launcher_walk_verdict;
use super::is_sensitive_target;
use super::readonly_pipeline::validate_readonly_pipeline;

pub(super) fn assess_pipeline(
    command: &str,
    parsed: ParsedCommand,
    policy: AssessmentPolicy,
) -> CommandAssessment {
    let mut impact = RiskImpact::Low;
    let mut confidence = AssessmentConfidence::High;
    let mut output_stability = OutputStability::StableSnapshot;
    let mut output_exposure = OutputExposure::Normal;
    let mut side_effects = Vec::new();
    let mut reasons = Vec::new();
    let mut any_unknown = false;
    let mut all_diagnostic = true;

    let mut has_upstream_network_output = false;
    let mut downloaded_files = Vec::new();
    let mut has_downloaded_code_execution = false;
    for stage_tokens in &parsed.stages {
        let program = stage_tokens
            .iter()
            .position(|token| !is_env_assignment(token))
            .and_then(|idx| stage_tokens.get(idx))
            .map(|token| basename(token).to_string());
        let Some(program) = program else {
            any_unknown = true;
            all_diagnostic = false;
            continue;
        };
        if stage_tokens.iter().any(|token| is_sensitive_target(token)) {
            impact = RiskImpact::High;
            output_exposure = OutputExposure::MayContainSecrets;
            side_effects.push(SideEffectClass::SensitiveDataRead);
            reasons.push("sensitive-path");
            all_diagnostic = false;
            continue;
        }
        // Launcher-chain walk (#2064): Unresolved caps certainty; a direct high-risk program is a zero-link chain.
        if let Some(walk) = walk_launcher_chain(stage_tokens) {
            if let Some((effects, stage_reasons, _)) = launcher_walk_verdict(&walk) {
                impact = RiskImpact::High;
                if matches!(walk, LauncherWalk::Unresolved { .. }) {
                    confidence = min_confidence(confidence, AssessmentConfidence::Medium);
                }
                side_effects.extend(effects);
                reasons.extend(stage_reasons);
                all_diagnostic = false;
                continue;
            }
        }
        if has_upstream_network_output
            && (matches!(program.as_str(), "sh" | "bash" | "zsh" | "fish")
                || interpreter_consumes_stdin_as_program(&program, stage_tokens))
        {
            has_downloaded_code_execution = true;
        }
        if downloaded_program_file(&program, stage_tokens)
            .is_some_and(|path| downloaded_files.iter().any(|downloaded| downloaded == path))
        {
            has_downloaded_code_execution = true;
        }
        if let Some((writes_stdout, output_files)) = network_download_effect(&program, stage_tokens)
        {
            has_upstream_network_output |= writes_stdout;
            downloaded_files.extend(output_files);
        }
        let stage = stage_assessment(&program, stage_tokens);
        impact = impact.max(stage.impact);
        confidence = min_confidence(confidence, stage.confidence);
        output_stability = max_output_stability(output_stability, stage.output_stability);
        output_exposure = max_output_exposure(output_exposure, stage.output_exposure);
        side_effects.extend(stage.side_effects);
        if !is_diagnostic_pipeline_stage(&program) {
            all_diagnostic = false;
        }
        if stage.reasons.contains(&"unknown-command") {
            any_unknown = true;
        }
        if stage.impact == RiskImpact::High {
            // Keep a high-impact stage's named reason (e.g.
            // `service-or-container-control`) in the verdict (PR #1790 review).
            reasons.extend(stage.reasons);
        }
    }

    let readonly_pipeline_evidence =
        policy.readonly_pipeline_executor && validate_readonly_pipeline(command).is_ok();

    if has_downloaded_code_execution {
        impact = RiskImpact::High;
        confidence = AssessmentConfidence::High;
        side_effects.push(SideEffectClass::RemoteCodeExecution);
        reasons.insert(0, "remote-code-execution");
    } else if readonly_pipeline_evidence {
        impact = RiskImpact::Low;
        confidence = AssessmentConfidence::High;
        reasons.insert(0, "readonly-pipeline-executor");
    } else if impact == RiskImpact::High {
        if reasons.is_empty() {
            reasons.push("pipeline-high-impact-stage");
        }
    } else if all_diagnostic || looks_like_diagnostic_pipeline(command) {
        impact = RiskImpact::Medium;
        confidence = min_confidence(confidence, AssessmentConfidence::Medium);
        reasons.insert(0, "diagnostic-pipeline-heuristic");
    } else {
        impact = RiskImpact::Medium;
        confidence = min_confidence(confidence, AssessmentConfidence::Medium);
        reasons.insert(0, "pipeline-not-auto-executable");
    }
    if any_unknown {
        confidence = min_confidence(confidence, AssessmentConfidence::Medium);
        reasons.push("unknown-stage");
    }
    reasons.push("pipeline-not-auto-executable");
    if side_effects.is_empty() {
        side_effects.push(SideEffectClass::None);
    }

    let auto_allow = if policy.auto_mode && readonly_pipeline_evidence && impact == RiskImpact::Low
    {
        Some(AutoAllowEvidence::ReadonlyPipelineExecutor)
    } else {
        None
    };
    let execution = if auto_allow.is_some() {
        ExecutionDecision::AutoAllow
    } else {
        ExecutionDecision::AskUser
    };

    assessment(
        policy.source,
        command,
        CommandShape::Pipeline,
        execution,
        impact,
        confidence,
        InteractionRequirement::None,
        output_stability,
        output_exposure,
        side_effects,
        dedupe_reasons(reasons),
        auto_allow,
    )
}

fn is_diagnostic_pipeline_stage(program: &str) -> bool {
    matches!(
        program,
        "df" | "ps" | "top" | "grep" | "rg" | "head" | "tail" | "sort" | "uniq" | "cut" | "wc"
    )
}

fn looks_like_diagnostic_pipeline(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    (lower.contains("ps ") || lower.starts_with("ps") || lower.contains("df "))
        && (lower.contains("| head") || lower.contains("| grep") || lower.contains("| sort"))
}
