pub(crate) mod broker;
pub(crate) mod classification;
pub(crate) mod command_risk;
mod command_risk_build;
mod command_risk_carried;
mod command_risk_compound;
mod command_risk_launcher;
mod command_risk_model;
mod command_risk_parser;
mod command_risk_pipeline;
#[cfg(test)]
mod command_risk_tests;
mod command_risk_verdict;
pub mod display;
pub(crate) mod guarded_diagnostic;
pub(crate) mod readonly_compound;
#[cfg(test)]
mod readonly_compound_tests;
pub(crate) mod readonly_interceptor;
pub(crate) mod readonly_pipeline;
pub(crate) mod readonly_rules;
mod temp_output;

pub(crate) fn strip_ansi(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek().is_some_and(|next| *next == '[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

pub(crate) fn is_sensitive_target(token: &str) -> bool {
    let token = token.trim_matches(|ch| ch == '"' || ch == '\'');
    let lower = token.to_ascii_lowercase();
    let basename = lower.rsplit('/').next().unwrap_or(lower.as_str());
    basename == ".env"
        || basename.starts_with(".env.")
        || basename == "id_rsa"
        || basename == "id_ed25519"
        || basename.ends_with(".pem")
        || basename.ends_with(".key")
        || basename.ends_with(".p12")
        || lower.contains(".ssh/")
        || lower.contains(".aws/credentials")
        || lower.contains(".config/gcloud")
        || lower.contains(".azure")
        || lower.contains(".kube/config")
        || lower.contains(".npmrc")
        || lower.contains(".pypirc")
        || lower.contains(".netrc")
        || lower == "/etc/shadow"
        || lower == "/etc/sudoers"
        || basename == ".zsh_history"
        || basename == ".bash_history"
        || basename == ".fish_history"
}

pub use broker::{apply_readonly_config, can_run_approved_bash_tool};
pub use classification::{
    canonical_tool_name, classify_command_interaction, is_readonly_builtin_tool_name,
    is_shell_tool_name, known_provider_tool, obvious_tty_command_reason, provider_tool_class,
    ApprovalRisk, CommandInteractionProfile, KnownProviderTool, OutputStability, ProviderToolClass,
    PtyRequirement,
};
pub use command_risk::{
    assess_shell_command, blocked_shell_binding_assessment, AssessmentConfidence, AssessmentPolicy,
    AssessmentSource, AssessmentSummary, AutoAllowEvidence, AutoExecutionPolicy,
    AutoExecutionRoute, CommandAssessment, CommandShape, ExecutionDecision, InteractionRequirement,
    OutputExposure, OutputStability as CommandRiskOutputStability, ReadonlyEvidence, RiskImpact,
    RiskReason, SideEffectClass,
};
pub use guarded_diagnostic::{
    run_guarded_diagnostic, validate_guarded_diagnostic, GuardedDiagnosticConfig,
    GuardedDiagnosticError, GuardedDiagnosticOutput, GuardedDiagnosticPlan,
};
pub use readonly_pipeline::{
    run_readonly_pipeline, validate_readonly_pipeline, ReadonlyPipelineConfig,
    ReadonlyPipelineError, ReadonlyPipelineOutput, ReadonlyPipelinePlan, ReadonlyPipelineStage,
};
