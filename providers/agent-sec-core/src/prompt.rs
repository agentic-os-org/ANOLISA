//! Scan the complete user text through the public fast-mode Prompt Scanner CLI.

use super::scan::Verdict;
use aw_exec::{CommandSpec, Limits};
use serde::Deserialize;
use serde_json::Value;
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct ScanResult {
    schema_version: String,
    ok: bool,
    verdict: String,
    input_truncated: bool,
    input_bytes_scanned: usize,
    degraded: bool,
    layers_failed: Vec<String>,
    layer_results: Vec<Layer>,
}

#[derive(Deserialize)]
struct Layer {
    layer: String,
}

pub(super) fn call(
    cli: &Path,
    socket: &Path,
    text: &str,
    context: &Value,
    timeout: Duration,
) -> Result<Verdict, &'static str> {
    // The CLI's stdin path strips Python whitespace before scanning.
    let scanned =
        text.trim_matches(|ch: char| ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch));
    if scanned.is_empty() {
        return Ok(Verdict::Pass);
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("invalid_budget")?;
    let timeout_ms = timeout.as_millis().min(u32::MAX as u128);
    if timeout_ms == 0 {
        return Err("deadline_exceeded");
    }
    let command = CommandSpec {
        program: cli.into(),
        args: vec![
            "--trace-context".into(),
            context.to_string().into(),
            "--socket".into(),
            socket.into(),
            "--timeout-ms".into(),
            timeout_ms.to_string().into(),
            "scan-prompt".into(),
            "--mode".into(),
            "fast".into(),
            "--source".into(),
            "user_input".into(),
            "--format".into(),
            "json".into(),
        ],
        cwd: std::env::current_dir().map_err(|_| "cli_context_error")?,
        environment: std::env::vars_os().collect(),
    };
    let output = aw_exec::run(
        &command,
        text.as_bytes(),
        Limits {
            input_bytes: aw_provider::MAX_MESSAGE_BYTES,
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
    if !output.status.success() {
        return Err("cli_failed");
    }
    let result: ScanResult =
        serde_json::from_slice(&output.stdout).map_err(|_| "invalid_prompt_result")?;
    if result.schema_version != "1.0"
        || result.input_truncated
        || result.degraded
        || result.input_bytes_scanned != scanned.len()
        || !result.layers_failed.is_empty()
        || !result
            .layer_results
            .iter()
            .any(|layer| layer.layer == "rule_engine")
    {
        return Err("incomplete_prompt_scan");
    }
    match (result.ok, result.verdict.as_str()) {
        (true, "pass") => Ok(Verdict::Pass),
        (false, "warn" | "deny") => Ok(Verdict::Risk),
        (_, "error") => Err("scan_error"),
        _ => Err("invalid_prompt_result"),
    }
}
