//! Shared stdin-only PII scanning for user and tool boundaries.

use aw_exec::{CommandSpec, Limits};
use serde::Deserialize;
use serde_json::Value;
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

/// Audit origin consumed by the public sec-core CLI.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// Text submitted by a user before a model request.
    UserInput,
    /// Arguments before native tool execution.
    ToolInput,
    /// Model-visible content after native tool execution.
    ToolOutput,
}

impl Source {
    /// Public CLI spelling of this origin.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserInput => "user_input",
            Self::ToolInput => "tool_input",
            Self::ToolOutput => "tool_output",
        }
    }
}

/// A completed scan decision, without findings or original content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No configured PII risk was detected.
    Pass,
    /// The backend reported warn or deny.
    Risk,
}

/// Scan failure remains distinct from a completed risk decision.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A bounded scan could not produce a complete, valid decision.
    #[error("PII scan failed: {code}")]
    Scan {
        /// Stable error code; never contains input or scanner diagnostics.
        code: &'static str,
    },
}

impl Error {
    /// Stable, content-free Provider error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Scan { code } => code,
        }
    }
}

impl From<&'static str> for Error {
    fn from(code: &'static str) -> Self {
        Self::Scan { code }
    }
}

#[derive(Deserialize)]
struct ScanResult {
    ok: bool,
    verdict: String,
    summary: Value,
}

/// Scan all UTF-8 text within the remaining invocation budget.
///
/// Text travels only through stdin. The caller owns cancellation of the Provider
/// process; `aw-exec` owns the CLI process group and joins it before returning.
/// Findings, CLI diagnostics and backend redacted text never enter the result.
///
/// # Errors
/// Reports transport, deadline, backend and incomplete/malformed scan failures.
pub fn call(
    cli: &Path,
    socket: &Path,
    text: &str,
    source: Source,
    trace_context: &Value,
    timeout: Duration,
) -> Result<Verdict, Error> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("invalid_budget")?;
    let timeout_ms = timeout.as_millis().min(u32::MAX as u128);
    if timeout_ms == 0 {
        return Err("deadline_exceeded".into());
    }
    let command = CommandSpec {
        program: cli.into(),
        args: vec![
            "--trace-context".into(),
            trace_context.to_string().into(),
            "--socket".into(),
            socket.into(),
            "--timeout-ms".into(),
            timeout_ms.to_string().into(),
            "scan-pii".into(),
            "--stdin".into(),
            "--format".into(),
            "json".into(),
            "--source".into(),
            source.as_str().into(),
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
        return Err("cli_failed".into());
    }
    let result: ScanResult =
        serde_json::from_slice(&output.stdout).map_err(|_| "invalid_scan_result")?;
    if !result.ok {
        return Err("scan_error".into());
    }
    let summary = &result.summary;
    if summary["source"] != source.as_str()
        || summary["truncated"] != false
        || summary["bytes_scanned"].as_u64() != Some(text.len() as u64)
        || summary
            .get("execution_status")
            .is_some_and(|s| s != "completed")
        || summary
            .get("coverage")
            .is_some_and(|s| s["status"] != "complete")
        || summary.get("custom_rules").is_some_and(|s| {
            !matches!(s["status"].as_str(), Some("absent" | "loaded"))
                || s["runtime_error_count"] != 0
                || s["budget_exhausted"] != false
                || s["truncated"] != false
        })
    {
        return Err("incomplete_pii_scan".into());
    }
    if Instant::now() >= deadline {
        return Err("deadline_exceeded".into());
    }
    match result.verdict.as_str() {
        "pass" => Ok(Verdict::Pass),
        "warn" | "deny" => Ok(Verdict::Risk),
        _ => Err("invalid_scan_result".into()),
    }
}
