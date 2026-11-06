//! Daemon-only task-to-Cedar proposals; every outcome is an unapproved candidate, never an
//! approval.

mod render;

use std::io::{self, Write as _};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use asc_daemon_protocol::{
    DaemonRequest, DaemonResponse, ErrorCode, error_code,
    intent::{
        INTENT_REQUEST_BYTES, INTENT_REQUEST_VERSION, INTENT_RESULT_BYTES, INTENT_TASK_BYTES,
        IntentProposeParams, IntentProposeResult,
    },
    method::INTENT_PROPOSE,
};
use chrono::Utc;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::Value;

/// Installs a SIGINT flag so an interrupted daemon call never looks like a completed run.
struct Interrupt(signal_hook::SigId);

impl Interrupt {
    fn register(flag: Arc<AtomicBool>) -> io::Result<Self> {
        signal_hook::flag::register(signal_hook::consts::SIGINT, flag).map(Self)
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.0);
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum IntentCommand {
    /// Generates a direct Cedar candidate; only the task text is sent to the configured model.
    Propose(Propose),
}

#[derive(Debug, Args)]
pub(crate) struct Propose {
    /// Complete task text; never interpreted as CLI options.
    #[arg(long, allow_hyphen_values = true)]
    task: String,
    /// Candidate lifetime in seconds, starting at the daemon request time.
    #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u32).range(1..=1800))]
    ttl: u32,
    /// Output format; printing a candidate never requests, records or implies approval.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
    Cedar,
}

/// Redacted intent failures without task text, file contents or raw transport errors.
#[derive(Debug, thiserror::Error)]
pub enum IntentFailure {
    /// Task, transport or output size exceeds its budget.
    #[error("intent input exceeds its budget; shorten the task text")]
    Limit,
    /// The candidate had already expired when it was received.
    #[error("{0}")]
    Clarification(&'static str),
    /// Local daemon transport failed; no local fallback is enabled.
    #[error("intent daemon transport failed; no candidate or local fallback")]
    Transport,
    /// Server returned a safe error code with an operator hint.
    #[error("intent daemon rejected request: {code}{hint}")]
    Daemon {
        /// Canonical wire error code.
        code: ErrorCode,
        /// Routing or configuration context for codes that usually mean a client mistake.
        hint: String,
    },
    /// Candidate or response failed integrity checks.
    #[error("invalid intent daemon response; no candidate rendered")]
    Response,
    /// Display or delivery failed.
    #[error("intent terminal or output failed")]
    Output,
    /// User interrupted the operation; no candidate is rendered.
    #[error("intent interrupted; no candidate rendered")]
    Interrupted,
}

impl IntentFailure {
    /// Returns 2 for clarification, 130 for interruption and 1 for other execution errors.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Clarification(_) => 2,
            Self::Interrupted => 130,
            _ => 1,
        }
    }
}

impl From<io::Error> for IntentFailure {
    fn from(_: io::Error) -> Self {
        Self::Output
    }
}

impl IntentCommand {
    pub(crate) fn run(&self, socket: &Path, timeout: Duration) -> Result<u8, IntentFailure> {
        let Self::Propose(options) = self;
        let interrupted = Arc::new(AtomicBool::new(false));
        let signal = Interrupt::register(Arc::clone(&interrupted))?;
        let result = options.run(socket, timeout, &interrupted);
        drop(signal);
        check_interrupt(&interrupted)?;
        result
    }
}

impl Propose {
    fn run(
        &self,
        socket: &Path,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<u8, IntentFailure> {
        check_interrupt(interrupted)?;
        if self.task.trim().is_empty() || self.task.len() > INTENT_TASK_BYTES {
            return Err(IntentFailure::Limit);
        }
        let result = self.propose(socket, timeout, interrupted, &self.task)?;
        check_interrupt(interrupted)?;
        if let IntentProposeResult::Candidate { candidate } = &result {
            render::validate(candidate, &self.task, self.ttl)?;
            fresh(candidate)?;
        }
        render::validate_draft(&result, &self.task)?;
        render::render(&result, &self.task, self.format)?;
        // A rendered result is a successful run even when the model and schema cannot express the
        // task: the candidate is informational, so callers must not read its exit code as
        // enforcement.
        Ok(0)
    }

    fn propose(
        &self,
        socket: &Path,
        timeout: Duration,
        interrupted: &AtomicBool,
        task: &str,
    ) -> Result<IntentProposeResult, IntentFailure> {
        let params = IntentProposeParams {
            format_version: INTENT_REQUEST_VERSION.into(),
            task: task.into(),
            ttl: self.ttl,
        };
        let request = DaemonRequest {
            method: INTENT_PROPOSE.into(),
            params: serde_json::to_value(params).map_err(|_| IntentFailure::Response)?,
            trace_context: None,
            compatibility: None,
        };
        // Reserve the newline; the server still checks line length after propagation injection.
        if serde_json::to_vec(&request)
            .map_err(|_| IntentFailure::Response)?
            .len()
            >= INTENT_REQUEST_BYTES
        {
            return Err(IntentFailure::Limit);
        }
        writeln!(
            io::stderr(),
            "The task text will be sent to the configured model and may incur charges; file contents are never read or sent."
        )?;
        // The call polls the SIGINT flag, so Ctrl-C ends the wait instead of waiting for the
        // response or the whole timeout.
        let response = asc_daemon_client::call_interruptible(socket, &request, timeout, || {
            interrupted.load(Ordering::Relaxed)
        })
        .map_err(|error| match error {
            asc_daemon_client::ClientError::Interrupted => IntentFailure::Interrupted,
            _ => IntentFailure::Transport,
        })?;
        let value = match response {
            DaemonResponse::Success(response) => response.result,
            DaemonResponse::Error(response) => {
                return Err(IntentFailure::Daemon {
                    hint: daemon_hint(&response.error.code, socket),
                    code: response.error.code,
                });
            }
        };
        if serde_json::to_vec(&value)
            .map_err(|_| IntentFailure::Response)?
            .len()
            > INTENT_RESULT_BYTES
        {
            return Err(IntentFailure::Response);
        }
        serde_json::from_value(value).map_err(|_| IntentFailure::Response)
    }
}

/// Names the socket and the usual causes when the daemon exposes no intent service.
///
/// An unavailable service covers both configuration and an unusable model answer, and an older
/// build answers `unknown_method`; without this hint the message never names the endpoint that
/// was actually contacted, which is the most common deployment mistake.
fn daemon_hint(code: &ErrorCode, socket: &Path) -> String {
    let socket = socket.display();
    match code.as_str() {
        error_code::UNAVAILABLE => format!(
            "; the intent service on {socket} is disabled or misconfigured, or the model returned no valid draft; check --socket or AGENT_SEC_DAEMON_SOCKET, ASC_INTENT_ENABLED=true and the model settings, then retry"
        ),
        error_code::UNKNOWN_METHOD => format!(
            "; {socket} does not register intent.propose, so it is an older daemon; check --socket or AGENT_SEC_DAEMON_SOCKET and restart the daemon built from this source tree"
        ),
        error_code::INVALID_ARGUMENT => "; check the task text, TTL and the request format version; use CLI and daemon built from the same source".into(),
        _ => String::new(),
    }
}

fn fresh(candidate: &Value) -> Result<(), IntentFailure> {
    if Utc::now().timestamp() >= render::expiry(candidate)? {
        Err(IntentFailure::Clarification(
            "Candidate expired; submit a new proposal. Expiry is never refreshed automatically.",
        ))
    } else {
        Ok(())
    }
}

fn check_interrupt(interrupted: &AtomicBool) -> Result<(), IntentFailure> {
    if interrupted.load(Ordering::Relaxed) {
        Err(IntentFailure::Interrupted)
    } else {
        Ok(())
    }
}
