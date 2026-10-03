//! Carried-payload classifier for the command-risk walker (#2064):
//! classifies the entire command string held by an inline-code option
//! (`su -c`, `sh -c`) or a rest-command carrier (`eval`) so a payload
//! program hidden behind a benign prefix cannot drop the verdict.

use super::command_risk::CommandShape;
use super::command_risk_launcher::{walk_launcher_chain, LauncherWalk};
use super::command_risk_parser::{is_env_assignment, parse_command};

/// Severity order used when a carried command holds several segments:
/// the worst segment decides the chain verdict.
fn walk_severity(walk: &LauncherWalk) -> u8 {
    match walk {
        LauncherWalk::SystemControl { .. } => 3,
        LauncherWalk::Unresolved { .. } => 2,
        LauncherWalk::Other { high: Some(_), .. } => 1,
        LauncherWalk::Other { high: None, .. } => 0,
    }
}

/// Reserved words that open shell control-flow constructs: a stage
/// headed by one executes whatever the construct decides at runtime
/// (`if true; then reboot; fi`), which a token-level walk cannot prove
/// safe — fail closed (I5).
const SHELL_RESERVED_WORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "while", "until", "do", "done", "for", "case", "esac",
    "in", "select", "function", "coproc", "!", "{", "}", "[[", "]]",
];

/// Classifies the entire command string carried by an inline-code
/// option (`su -c`, `sh -c`) or a rest-command carrier (`eval`). Every
/// compound segment and pipeline stage is walked, and the worst verdict
/// wins, so `sh -c 'echo ok; reboot'` keeps the reboot payload instead
/// of being judged by its first word alone (#2064).
pub(super) fn classify_carried_command(command: &str, escalated: bool) -> LauncherWalk {
    let parsed = parse_command(command);
    // Opaque payloads fail closed (I5): command substitution executes
    // its inner command during expansion (`echo $(reboot)`), Complex
    // shapes hide executables behind grouping or background operators,
    // and Unparseable means nothing can be proven — in every case the
    // verdict may not drop below the high-risk gate.
    if matches!(
        parsed.shape,
        CommandShape::Unparseable | CommandShape::CommandSubstitution | CommandShape::Complex
    ) {
        return LauncherWalk::Unresolved { escalated };
    }
    // Compound payloads carry per-segment stage lists; simple and
    // pipeline payloads keep their stages flat.
    let segments: Vec<&[Vec<String>]> = if parsed.segments.is_empty() {
        vec![parsed.stages.as_slice()]
    } else {
        parsed.segments.iter().map(Vec::as_slice).collect()
    };
    let mut worst: Option<LauncherWalk> = None;
    for stages in segments {
        for stage in stages {
            let start = stage
                .iter()
                .position(|token| !is_env_assignment(token))
                .unwrap_or(stage.len());
            let stage_tokens = &stage[start..];
            // A control-flow construct decides the executed command at
            // runtime: the stage cannot be resolved token-wise (I5).
            if stage_tokens
                .first()
                .is_some_and(|head| SHELL_RESERVED_WORDS.contains(&head.as_str()))
            {
                return LauncherWalk::Unresolved { escalated };
            }
            let Some(walk) = walk_launcher_chain(stage_tokens) else {
                continue;
            };
            if worst
                .as_ref()
                .is_none_or(|current| walk_severity(&walk) >= walk_severity(current))
            {
                worst = Some(walk);
            }
        }
    }
    match worst {
        Some(mut walk) => {
            // Escalation earned by the outer chain is never dropped (I3).
            match &mut walk {
                LauncherWalk::SystemControl { escalated: inner }
                | LauncherWalk::Unresolved { escalated: inner }
                | LauncherWalk::Other {
                    escalated: inner, ..
                } => *inner |= escalated,
            }
            walk
        }
        // Every segment resolved to an ordinary program: the carrier
        // contributes nothing and the caller's verdict applies (I3).
        None => LauncherWalk::Other {
            escalated,
            high: None,
        },
    }
}
