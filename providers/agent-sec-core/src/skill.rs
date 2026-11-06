//! Check the Adapter-resolved Skill through the public read-only Ledger command.

use std::{
    path::{Component, Path},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use aw_exec::{CommandSpec, Limits};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Skill {
    name: String,
    directory: String,
    source: String,
}

#[derive(Deserialize)]
struct Check {
    status: String,
}

pub(super) fn call(
    cli: &Path,
    socket: &Path,
    event: &Value,
    timeout: Duration,
) -> Result<&'static str, &'static str> {
    let skill: Skill = serde_json::from_value(event["native"]["aw_skill"].clone())
        .map_err(|_| "skill_resolution_error")?;
    let directory = Path::new(&skill.directory);
    if event["tool"]["input"]["skill"].as_str() != Some(skill.name.as_str())
        || skill.name.is_empty()
        || skill.name.len() > 1024
        || skill.source.is_empty()
        || skill.source.len() > 128
        || !directory.is_absolute()
        || directory.parent().is_none()
        || skill.directory.len() > 4096
        || skill.directory.contains('\0')
        || directory
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("skill_resolution_error");
    }
    let timeout_ms = timeout.as_millis().min(u32::MAX as u128);
    if timeout_ms == 0 {
        return Err("deadline_exceeded");
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("invalid_budget")?;
    let command = CommandSpec {
        program: cli.into(),
        args: vec![
            "--trace-context".into(),
            super::trace_context::from_event(event).to_string().into(),
            "--socket".into(),
            socket.into(),
            "--timeout-ms".into(),
            timeout_ms.to_string().into(),
            "skill-ledger".into(),
            "check".into(),
            skill.directory.into(),
        ],
        cwd: std::env::current_dir().map_err(|_| "cli_context_error")?,
        environment: std::env::vars_os().collect(),
    };
    let output = aw_exec::run(
        &command,
        &[],
        Limits {
            input_bytes: 0,
            stdout_bytes: 1024 * 1024,
            stderr_bytes: 65536,
        },
        deadline,
        &AtomicBool::new(false),
    )
    .map_err(|error| match error {
        aw_exec::Error::DeadlineExceeded => "deadline_exceeded",
        aw_exec::Error::OutputLimit { .. } => "cli_output_limit",
        aw_exec::Error::Cleanup { .. } => "cli_cleanup_error",
        _ => "cli_transport_error",
    })?;
    // Ledger uses exit 1 for completed checks with negative trust states as
    // well as backend errors. Only a recognized, consistent state is a verdict.
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err("cli_failed");
    }
    let check: Check =
        serde_json::from_slice(&output.stdout).map_err(|_| "invalid_skill_result")?;
    match (output.status.code(), check.status.as_str()) {
        (Some(0), "pass") => Ok("skill_pass"),
        (_, "none") => Ok("skill_unsigned"),
        (_, "drifted") => Ok("skill_drifted"),
        (_, "warn") => Ok("skill_warn"),
        (_, "deny") => Ok("skill_deny"),
        (_, "tampered") => Ok("skill_tampered"),
        (_, "error") => Err("skill_check_error"),
        _ => Err("invalid_skill_result"),
    }
}
