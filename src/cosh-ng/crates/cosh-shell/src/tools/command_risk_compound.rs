use super::command_risk::{
    assess_simple_command, insert_structural_reason, is_high_risk_explanation,
    AssessmentConfidence, AssessmentPolicy, AutoAllowEvidence, CommandAssessment, CommandShape,
    ExecutionDecision, InteractionRequirement, OutputExposure, OutputStability, RiskImpact,
    SideEffectClass,
};
use super::command_risk_build::{
    dedupe_reasons, max_output_exposure, max_output_stability, min_confidence,
};
use super::command_risk_parser::ParsedCommand;
use super::command_risk_pipeline::assess_pipeline;
use super::readonly_compound::build_readonly_compound_plan;

/// Returns the per-segment pipeline stages for all compound commands
/// (`&&`/`||`/`;`/newline separated, issue #1785): every segment is
/// assessed individually and the results aggregated so high-risk tails
/// keep their full stage assessment instead of being masked by the
/// first segment. A bare pipeline masked by an input redirection
/// (`cat < in | rm ...`, where `RedirectionRead` outranks `Pipeline` as
/// dominant shape) has no segment separators, so all of its stages
/// become a single pipeline segment. Returns `None` for non-compound
/// shapes and for single-stage `RedirectionRead` commands, which keep
/// their shape-specific paths.
pub(super) fn compound_segments(parsed: &ParsedCommand) -> Option<Vec<Vec<Vec<String>>>> {
    if !matches!(
        parsed.shape,
        CommandShape::AndOrList | CommandShape::Sequence | CommandShape::RedirectionRead
    ) {
        return None;
    }
    if !parsed.segments.is_empty() {
        return Some(parsed.segments.clone());
    }
    (parsed.stages.len() > 1).then(|| vec![parsed.stages.clone()])
}

/// Assesses a compound command (`&&` / `||` / `;` / newline separated)
/// by re-using the existing simple/pipeline assessment per segment and
/// aggregating the results: impact takes the maximum, confidence the
/// minimum, and reasons are union-deduplicated (issue #1785). Assessing
/// per recorded segment keeps command/argument boundaries, unlike the
/// earlier word-scan compensation (PR #1790 review) which both missed
/// rules that need full stage assessment (`kubectl delete`, `docker
/// run`, `awk system()`, `curl | sh`) and escalated benign arguments
/// (`echo rm>/dev/null && true`).
///
/// The compound execution boundary is relaxed in exactly one case
/// (issue #1882): a readonly-compound execution plan exists for the
/// whole command (see `build_readonly_compound_plan`), granting
/// `CompoundReadonly` evidence in auto mode. Every other compound keeps
/// `AskUser`, never auto-allow. Assessment aggregation is unchanged.
pub(super) fn assess_stripped_compound(
    command: &str,
    shape: CommandShape,
    segments: &[Vec<Vec<String>>],
    policy: AssessmentPolicy,
) -> CommandAssessment {
    let mut impact = RiskImpact::Low;
    let mut confidence = AssessmentConfidence::High;
    let mut interaction = InteractionRequirement::None;
    let mut output_stability = OutputStability::StableSnapshot;
    let mut output_exposure = OutputExposure::Normal;
    let mut side_effects: Vec<SideEffectClass> = Vec::new();
    let mut reasons: Vec<&'static str> = Vec::new();

    for segment in segments {
        let segment_text = segment
            .iter()
            .map(|stage| stage.join(" "))
            .collect::<Vec<_>>()
            .join(" | ");
        let parsed = ParsedCommand {
            shape: if segment.len() > 1 {
                CommandShape::Pipeline
            } else {
                CommandShape::Simple
            },
            stages: segment.clone(),
            requires_shell_expansion: false,
            null_redirections: Default::default(),
            segments: Vec::new(),
            segment_connectors: Vec::new(),
        };
        let assessed = if segment.len() > 1 {
            assess_pipeline(&segment_text, parsed, policy)
        } else {
            assess_simple_command(&segment_text, parsed, policy)
        };
        impact = impact.max(assessed.impact);
        confidence = min_confidence(confidence, assessed.confidence);
        interaction = max_interaction(interaction, assessed.interaction);
        output_stability = max_output_stability(output_stability, assessed.output_stability);
        output_exposure = max_output_exposure(output_exposure, assessed.output_exposure);
        for side_effect in assessed.side_effects {
            if !side_effects.contains(&side_effect) {
                side_effects.push(side_effect);
            }
        }
        reasons.extend(assessed.reasons);
    }

    let mut reasons = dedupe_reasons(reasons);
    if impact == RiskImpact::High {
        // Keep a high-risk explanation as the primary reason so the
        // approval card renders the matching phrase (ARP SDD design §4).
        if let Some(position) = reasons
            .iter()
            .position(|reason| is_high_risk_explanation(reason))
        {
            let primary = reasons.remove(position);
            reasons.insert(0, primary);
        }
    }
    let auto_allow = compound_readonly_evidence(command).filter(|_| policy.auto_mode);
    if auto_allow.is_some() {
        // The whole compound is auto-executable; the structural
        // "not-auto-executable" reason no longer applies.
        reasons.insert(0, "compound-readonly");
    } else {
        insert_structural_reason(
            &mut reasons,
            match shape {
                CommandShape::AndOrList => "and-or-list-not-auto-executable",
                CommandShape::Sequence => "sequence-not-auto-executable",
                CommandShape::RedirectionRead => "read-redirection-not-auto-executable",
                _ => "complex-shell-not-auto-executable",
            },
        );
    }

    CommandAssessment {
        source: policy.source,
        command: command.to_string(),
        shape,
        execution: if auto_allow.is_some() {
            ExecutionDecision::AutoAllow
        } else {
            ExecutionDecision::AskUser
        },
        impact,
        confidence: min_confidence(confidence, AssessmentConfidence::Medium),
        interaction,
        output_stability,
        output_exposure,
        side_effects,
        reasons,
        auto_allow,
    }
}

/// Grants `CompoundReadonly` evidence (issue #1882) exactly when an
/// execution plan exists for the compound: eligibility is decided by
/// `build_readonly_compound_plan`, the same function the consumer uses
/// to build the plan it runs, so the assessment path and the execution
/// path can never disagree about what would run. The executor spawns
/// parser tokens directly with `std::process::Command` — no shell
/// parsing layer ever touches the assessed text, so quote/escape/newline
/// forms stay eligible (the parser preserves token boundaries) and no
/// expansion mechanism (history, glob, tilde, parameter, alias) can
/// fire: the assessed token sequence *is* the executed argv.
///
/// Anything without a plan falls through to the unchanged `AskUser`
/// path, so the worst case of an unenumerated form is over-conservatism.
/// Short-circuit semantics only decide which segments run, never their
/// argv, and every segment carries its own evidence, so any executed
/// subset stays within the assessed set.
fn compound_readonly_evidence(command: &str) -> Option<AutoAllowEvidence> {
    build_readonly_compound_plan(command).map(|_| AutoAllowEvidence::CompoundReadonly)
}

/// Applies the conservative `Complex` classification: floor the impact
/// at Medium, force Low confidence, and fail closed to High when the
/// command splits into more than one segment (issue #1785 review) —
/// subshell/brace/background syntax cannot be reliably segmented, so
/// tail segments stay invisible to the first-stage assessment and the
/// risk must not be understated. The execution boundary (`AskUser`) is
/// untouched.
pub(super) fn finalize_complex(assessment: &mut CommandAssessment, parsed: &ParsedCommand) {
    assessment.execution = ExecutionDecision::AskUser;
    assessment.confidence = AssessmentConfidence::Low;
    if assessment.impact < RiskImpact::Medium {
        assessment.impact = RiskImpact::Medium;
    }
    if parsed.segments.len() > 1 {
        assessment.impact = RiskImpact::High;
        assessment.reasons.push("unsplittable-compound");
    }
    insert_structural_reason(&mut assessment.reasons, "complex-shell-not-auto-executable");
}

fn max_interaction(
    left: InteractionRequirement,
    right: InteractionRequirement,
) -> InteractionRequirement {
    use InteractionRequirement::*;
    let rank = |interaction| match interaction {
        None => 0,
        TtyRequired => 1,
        CredentialPromptLikely => 2,
    };
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}
