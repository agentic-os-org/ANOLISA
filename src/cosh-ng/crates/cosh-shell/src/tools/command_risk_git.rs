//! Capability-based disclosure of git subcommands that can execute
//! repo-embedded config (diff.external, textconv, fsmonitor, sshCommand,
//! repo-configured signature verification for log/shortlog, and patch
//! forms of stash).
use super::command_risk::{
    insert_structural_reason, AssessmentConfidence, RiskImpact, StageAssessment,
};
use super::command_risk_build::basename;
use super::command_risk_parser::is_env_assignment;
use super::readonly_interceptor::interceptor_payload_for_assessment;

const GIT_REPO_CONFIG_SUBCOMMANDS: &[&str] = &[
    "diff", "show", "blame", "describe", "status", "ls-files", "reflog", "remote", "log",
    "shortlog", "stash",
];
const GIT_GLOBAL_VALUE_FLAGS: &[&str] = &[
    "-c",
    "-C",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--config-env",
    "--exec-path",
    "--attr-source",
];
const GIT_GLOBAL_PLAIN_FLAGS: &[&str] = &[
    "--no-pager",
    "--paginate",
    "-P",
    "-p",
    "--bare",
    "--no-replace-objects",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--no-optional-locks",
    "--no-lazy-fetch",
    "--no-advice",
];

// Capability-based and fail-closed: without cwd or repo config the classifier
// cannot prove a form hook-free, so every approval-bound family form discloses.
pub(super) fn git_repo_config_reason(tokens: &[String]) -> Option<&'static str> {
    let mut tokens = tokens;
    loop {
        if tokens.first().is_some_and(|token| token == "env") {
            let rest = &tokens[1..];
            let end = rest
                .iter()
                .position(|token| !is_env_assignment(token))
                .unwrap_or(rest.len());
            tokens = &rest[end..];
        }
        match interceptor_payload_for_assessment(tokens) {
            Some(payload) => tokens = payload,
            None => break,
        }
    }
    if tokens.first().map(|token| basename(token)) != Some("git") {
        return None;
    }
    let mut args = tokens.iter().skip(1);
    while let Some(token) = args.next() {
        if token.starts_with('-') {
            let (flag, inline) = match token.split_once('=') {
                Some((flag, _)) => (flag, true),
                None => (token.as_str(), false),
            };
            if GIT_GLOBAL_VALUE_FLAGS.contains(&flag) {
                if !inline {
                    args.next();
                }
                continue;
            }
            if GIT_GLOBAL_PLAIN_FLAGS.contains(&flag) {
                continue;
            }
            return None;
        }
        if GIT_REPO_CONFIG_SUBCOMMANDS.contains(&token.as_str()) {
            return Some("git-repo-config-execution");
        }
        return None;
    }
    None
}

pub(super) fn disclose_stage_git_repo_config(
    stage: &mut StageAssessment,
    tokens: &[String],
) -> bool {
    match git_repo_config_reason(tokens) {
        Some(reason) => {
            stage.impact = RiskImpact::High;
            stage.confidence = AssessmentConfidence::High;
            insert_structural_reason(&mut stage.reasons, reason);
            true
        }
        None => false,
    }
}
