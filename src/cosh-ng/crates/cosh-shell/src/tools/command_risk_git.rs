//! Capability-based disclosure of git subcommands that can execute
//! repo-embedded config: diff.external, textconv, fsmonitor, sshCommand,
//! signature verification (log/shortlog, ref listing sort/format atoms,
//! tag/commit verification, verify-tag/verify-commit), plumbing patch
//! generation (diff-tree/diff-index/diff-files), and object reads that
//! lazy-fetch from the repo-configured remote in partial clones.
use super::command_risk::{
    insert_structural_reason, AssessmentConfidence, RiskImpact, StageAssessment,
};
use super::command_risk_build::basename;
use super::command_risk_launcher::{launcher_payload, LauncherPayload};
use super::command_risk_parser::{is_env_assignment, parse_command};
use super::readonly_interceptor::interceptor_payload_for_assessment;

const GIT_REPO_CONFIG_SUBCOMMANDS: &[&str] = &[
    "diff",
    "diff-tree",
    "diff-index",
    "diff-files",
    "show",
    "blame",
    "describe",
    "status",
    "ls-files",
    "reflog",
    "remote",
    "log",
    "shortlog",
    "stash",
    "branch",
    "tag",
    "verify-tag",
    "verify-commit",
    "for-each-ref",
    "name-rev",
    "rev-parse",
    "rev-list",
    "ls-tree",
    "cat-file",
    "config",
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
    let program_start = tokens
        .iter()
        .position(|token| !is_env_assignment(token))
        .unwrap_or(tokens.len());
    tokens = &tokens[program_start..];
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
            // remote bare/-v/get-url are the empirically hook-free kept
            // forms; write/network subcommands still disclose.
            if token == "remote" {
                let rest: Vec<&str> = args.map(|arg| arg.as_str()).collect();
                match rest.as_slice() {
                    [] | ["-v"] | ["--verbose"] | ["get-url", _] => return None,
                    _ => return Some("git-repo-config-execution"),
                }
            }
            if token == "config" {
                // Object-source reads lazy-fetch from the repo-configured remote.
                // Git accepts unambiguous long-option abbreviations (--bl/--blo
                // resolve to --blob); match them fail-closed. Ordinary config
                // queries keep the generic card.
                let has_blob = args.clone().any(|arg| {
                    let flag = arg.split('=').next().unwrap_or(arg);
                    flag == "--bl" || flag == "--blo" || flag == "--blob"
                });
                return if has_blob {
                    Some("git-repo-config-execution")
                } else {
                    None
                };
            }
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
    match launcher_git_repo_config_reason(tokens) {
        Some(reason) => {
            stage.impact = RiskImpact::High;
            stage.confidence = AssessmentConfidence::High;
            insert_structural_reason(&mut stage.reasons, reason);
            true
        }
        None => false,
    }
}

/// Reason for a stage that may sit behind launchers, including the
/// command string an inline-code carrier holds (`su -c 'git diff'`,
/// `sh -c 'git diff'`, `eval …`). The launcher walk decides where the
/// payload lives; a carried command is parsed again and every segment
/// and pipeline stage is checked, so `eval 'echo ok; git status'`
/// discloses like the naked command.
pub(super) fn launcher_git_repo_config_reason(tokens: &[String]) -> Option<&'static str> {
    match launcher_payload(tokens) {
        Some(LauncherPayload::Program(offset)) => git_repo_config_reason(&tokens[offset..]),
        Some(LauncherPayload::Carried(command)) => carried_git_repo_config_reason(&command),
        None => git_repo_config_reason(tokens),
    }
}

fn carried_git_repo_config_reason(command: &str) -> Option<&'static str> {
    let parsed = parse_command(command);
    let segments: Vec<&[Vec<String>]> = if parsed.segments.is_empty() {
        vec![parsed.stages.as_slice()]
    } else {
        parsed.segments.iter().map(Vec::as_slice).collect()
    };
    segments
        .into_iter()
        .flatten()
        .find_map(|stage| launcher_git_repo_config_reason(stage))
}
