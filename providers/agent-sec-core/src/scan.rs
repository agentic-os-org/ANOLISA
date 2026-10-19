//! Bound CLI transport and project verdicts without exposing scan evidence.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use aw_exec::{CommandSpec, Limits};
use serde::Deserialize;

use super::config::Language;

pub(super) enum Verdict {
    Pass,
    Risk,
}

// Consume only the public decision fields. Additive CLI output fields do not
// change this bridge; raw findings and diagnostics never enter AW responses.
#[derive(Deserialize)]
struct ScanResult {
    ok: bool,
    verdict: String,
}

pub(super) fn call(
    cli: &Path,
    socket: &Path,
    code: &str,
    language: Language,
    timeout: Duration,
) -> Result<Verdict, &'static str> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("invalid_budget")?;
    // Floor the timeout: the CLI's own daemon call must not extend our budget.
    let timeout_ms = timeout.as_millis().min(u32::MAX as u128);
    if timeout_ms == 0 {
        return Err("deadline_exceeded");
    }
    let command = CommandSpec {
        program: cli.into(),
        args: vec![
            "--socket".into(),
            socket.into(),
            "--timeout-ms".into(),
            timeout_ms.to_string().into(),
            "scan-code".into(),
            "--code".into(),
            code.into(),
            "--language".into(),
            language.as_str().into(),
            "--mode".into(),
            "regex".into(),
        ],
        cwd: std::env::current_dir().map_err(|_| "cli_context_error")?,
        // The Host already selected this Provider's environment. Preserve it
        // for the CLI (e.g. locale/telemetry); never interpolate it into argv.
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
    if !output.status.success() {
        // A daemon/usage/scan failure is never a security policy block, even
        // when stdout happens to contain a plausible verdict.
        return Err("cli_failed");
    }
    let result: ScanResult =
        serde_json::from_slice(&output.stdout).map_err(|_| "invalid_scan_result")?;
    match (result.ok, result.verdict.as_str()) {
        (true, "pass") => Ok(Verdict::Pass),
        // CLI exit 0 means scanning succeeded; warn/deny still indicate risk.
        (true, "warn" | "deny") => Ok(Verdict::Risk),
        (false, "error") => Err("scan_error"),
        _ => Err("invalid_scan_result"),
    }
}
