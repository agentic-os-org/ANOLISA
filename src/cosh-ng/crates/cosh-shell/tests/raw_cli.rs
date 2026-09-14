use std::fs;
use std::io::Write;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use wait_timeout::ChildExt;

#[path = "raw_cli/activity.rs"]
mod activity;
#[path = "raw_cli/agent_input.rs"]
mod agent_input;
#[path = "raw_cli/animation.rs"]
mod animation;
#[path = "raw_cli/approval.rs"]
mod approval;
#[path = "raw_cli/audit.rs"]
mod audit;
#[path = "raw_cli/auth.rs"]
mod auth;
#[path = "raw_cli/cancellation.rs"]
mod cancellation;
#[path = "raw_cli/compaction.rs"]
mod compaction;
#[path = "raw_cli/composer.rs"]
mod composer;
#[path = "raw_cli/config.rs"]
mod config;
#[path = "raw_cli/cosh_core/mod.rs"]
mod cosh_core;
#[path = "raw_cli/diagnostics.rs"]
mod diagnostics;
#[path = "raw_cli/doctor.rs"]
mod doctor;
#[path = "raw_cli/evidence_request.rs"]
mod evidence_request;
#[path = "raw_cli/external_hook.rs"]
mod external_hook;
#[path = "raw_cli/failed_command.rs"]
mod failed_command;
#[path = "raw_cli/heavy.rs"]
mod heavy;
#[path = "raw_cli/host_executed.rs"]
mod host_executed;
#[path = "raw_cli/i18n.rs"]
mod i18n;
#[path = "raw_cli/memory_hook.rs"]
mod memory_hook;
#[path = "raw_cli/mode.rs"]
mod mode;
#[path = "raw_cli/native.rs"]
mod native;
#[path = "raw_cli/passthrough.rs"]
mod passthrough;
#[path = "raw_cli/prompt_replay.rs"]
mod prompt_replay;
#[path = "raw_cli/provider_handoff/mod.rs"]
mod provider_handoff;
#[path = "raw_cli/provider_tools.rs"]
mod provider_tools;
#[path = "raw_cli/question.rs"]
mod question;
#[path = "raw_cli/recommendation.rs"]
mod recommendation;
#[path = "raw_cli/registry.rs"]
mod registry;
#[path = "raw_cli/renderer.rs"]
mod renderer;
#[path = "raw_cli/session.rs"]
mod session;
#[path = "raw_cli/slash.rs"]
mod slash;
#[path = "raw_cli/startup.rs"]
mod startup;
#[path = "support/mod.rs"]
mod support;
#[path = "raw_cli/task.rs"]
mod task;
#[cfg(target_os = "linux")]
#[path = "raw_cli/terminal_ownership.rs"]
mod terminal_ownership;
#[cfg(target_os = "linux")]
#[path = "raw_cli/terminal_resize.rs"]
mod terminal_resize;
#[cfg(target_os = "linux")]
#[path = "raw_cli/terminal_unicode.rs"]
mod terminal_unicode;

pub(crate) use i18n::*;
use support::raw_cli::*;

fn approval_request_card_visible(output: &str) -> bool {
    output.contains("Approval req-")
        || output.contains("审批 req-")
        || output.contains("Approval required")
        || output.contains("需要审批")
}

fn assert_no_approval_request_card(output: &str) {
    assert!(!approval_request_card_visible(output), "{output}");
}

fn bash_supports_command_not_found_handler() -> bool {
    Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-ic",
            "command_not_found_handle(){ return 0; }; __cosh_missing_probe",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub(crate) fn assert_approval_prompt_visible(output: &str) {
    assert!(
        output.contains("Approval required") || output.contains("Approval req-1"),
        "{output}"
    );
}

pub(crate) fn assert_zh_approval_prompt_visible(output: &str) {
    assert!(
        output.contains("需要审批") || output.contains("审批 req-1"),
        "{output}"
    );
}

pub(crate) fn count_approval_prompts(output: &str) -> usize {
    count_occurrences(output, "Approval required") + count_occurrences(output, "Approval req-")
}

pub(crate) fn count_zh_approval_prompts(output: &str) -> usize {
    count_occurrences(output, "需要审批") + count_occurrences(output, "审批 req-")
}

pub(crate) fn ls_ccc_failure_analysis(output: &str) -> Option<&'static str> {
    [
        "The command ls ccc failed with exit code 1.",
        "The command ls ccc failed with exit code 2.",
    ]
    .into_iter()
    .find(|message| output.contains(message))
}
