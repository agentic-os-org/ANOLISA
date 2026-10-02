//! Maps launcher-chain walk outcomes to assessment verdicts (#2064):
//! the adapter layer between the token-level walker and the shared
//! simple/pipeline assessment paths.

use super::command_risk::{
    AssessmentConfidence, AssessmentSource, CommandAssessment, CommandShape, ExecutionDecision,
    InteractionRequirement, OutputExposure, OutputStability, RiskImpact, SideEffectClass,
};
use super::command_risk_build::assessment;
use super::command_risk_launcher::{walk_launcher_chain, LauncherWalk};

/// Side effects / reasons / interaction for a resolved launcher walk,
/// shared by the simple and pipeline paths. Returns `None` for a chain
/// that resolves to an ordinary program without escalation — the
/// caller's existing verdict applies unchanged.
pub(super) fn launcher_walk_verdict(
    walk: &LauncherWalk,
) -> Option<(
    Vec<SideEffectClass>,
    Vec<&'static str>,
    InteractionRequirement,
)> {
    match walk {
        LauncherWalk::SystemControl { escalated } => Some(if *escalated {
            (
                vec![
                    SideEffectClass::PrivilegeEscalation,
                    SideEffectClass::SystemControl,
                ],
                vec!["privilege-escalation", "system-control"],
                InteractionRequirement::CredentialPromptLikely,
            )
        } else {
            (
                vec![SideEffectClass::SystemControl],
                vec!["system-control"],
                InteractionRequirement::None,
            )
        }),
        LauncherWalk::Other { escalated, high } => match high {
            Some((side_effect, reason, interaction)) => Some(if *escalated {
                // A nested-escalation payload (`su -c "sudo reboot"`)
                // would list PrivilegeEscalation twice; collapse.
                let mut effects = vec![SideEffectClass::PrivilegeEscalation, *side_effect];
                effects.dedup();
                let mut reasons = vec!["privilege-escalation", *reason];
                reasons.dedup();
                (
                    effects,
                    reasons,
                    InteractionRequirement::CredentialPromptLikely,
                )
            } else {
                (vec![*side_effect], vec![*reason], *interaction)
            }),
            None if *escalated => Some((
                vec![SideEffectClass::PrivilegeEscalation],
                vec!["privilege-escalation"],
                InteractionRequirement::CredentialPromptLikely,
            )),
            None => None,
        },
        LauncherWalk::Unresolved { escalated } => Some(if *escalated {
            (
                vec![SideEffectClass::PrivilegeEscalation],
                vec!["privilege-escalation", "unresolvable-launcher-chain"],
                InteractionRequirement::CredentialPromptLikely,
            )
        } else {
            (
                vec![SideEffectClass::Unknown],
                vec!["unresolvable-launcher-chain"],
                InteractionRequirement::None,
            )
        }),
    }
}

pub(super) fn high_risk_program_assessment(
    source: AssessmentSource,
    command: &str,
    shape: CommandShape,
    _program: &str,
    tokens: &[String],
) -> Option<CommandAssessment> {
    // The launcher-chain walk doubles as the high-risk arm (#2064): a
    // direct high-risk program is a zero-link chain, and launcher-wrapped
    // forms resolve through the same arity-aware walker, so wrapped
    // `env rm -rf …` keeps the payload's verdict instead of falling
    // through to the unknown-command fallback.
    let walk = walk_launcher_chain(tokens)?;
    let (side_effects, reasons, interaction) = launcher_walk_verdict(&walk)?;
    // Unresolved chains carry less parsing certainty than resolved ones.
    let confidence = match walk {
        LauncherWalk::Unresolved { .. } => AssessmentConfidence::Medium,
        _ => AssessmentConfidence::High,
    };
    Some(assessment(
        source,
        command,
        shape,
        ExecutionDecision::AskUser,
        RiskImpact::High,
        confidence,
        interaction,
        OutputStability::StableSnapshot,
        if side_effects.contains(&SideEffectClass::CredentialAccess) {
            OutputExposure::MayContainSecrets
        } else {
            OutputExposure::Normal
        },
        side_effects,
        reasons,
        None,
    ))
}
