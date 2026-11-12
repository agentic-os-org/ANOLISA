//! Task- and command-to-Cedar proposals through the daemon plus local rule-to-Cedar init; every
//! outcome is an unapproved candidate, never an approval.

mod command;
mod render;

use std::io::{self, Read as _, Write as _};
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
use asc_intent::{IntentSpec, compile_spec};
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
    /// Derives the Cedar a single hook-observed command needs; sends only command and cwd.
    ProposeCommand(command::ProposeCommand),
    /// Generates validated Cedar locally from explicit rules; no daemon, model or network.
    Init(Init),
}

/// Local init output budget for a spec file read from disk or stdin.
const SPEC_FILE_BYTES: usize = 256 * 1024;
/// Version of the local init JSON output; it is a preview, not a candidate.
const INIT_VERSION: &str = "asc.intent.init.v1";

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

/// Explicit allow/deny rules compiled to Cedar on this host; forbid takes precedence over
/// permit, and anything no permit names stays denied by default.
#[derive(Debug, Args)]
pub(crate) struct Init {
    /// Path granted file.read; repeatable. A trailing /* or /** grants the whole subtree.
    #[arg(long, value_name = "PATH")]
    allow_read: Vec<String>,
    /// Path granted file.write; repeatable. A trailing /* or /** grants the whole subtree.
    #[arg(long, value_name = "PATH")]
    allow_write: Vec<String>,
    /// Executable path granted file.exec; repeatable.
    #[arg(long, value_name = "PATH")]
    allow_exec: Vec<String>,
    /// Path denied file.read, carving an exception out of broader allows; repeatable.
    #[arg(long, value_name = "PATH")]
    deny_read: Vec<String>,
    /// Path denied file.write; repeatable.
    #[arg(long, value_name = "PATH")]
    deny_write: Vec<String>,
    /// Executable path denied file.exec; repeatable.
    #[arg(long, value_name = "PATH")]
    deny_exec: Vec<String>,
    /// Grant network.use for the whole network; the schema has no endpoint granularity.
    #[arg(long)]
    allow_network: bool,
    /// Deny network.use for the whole network.
    #[arg(long, conflicts_with = "allow_network")]
    deny_network: bool,
    /// Restrict read/write rules to this requesting program; repeatable, exact paths only.
    #[arg(long, value_name = "PATH")]
    program: Vec<String>,
    /// JSON spec file, or - for stdin; mutually exclusive with the rule flags.
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with_all = [
            "allow_read", "allow_write", "allow_exec",
            "deny_read", "deny_write", "deny_exec",
            "allow_network", "deny_network", "program",
        ],
    )]
    spec_file: Option<String>,
    /// Output format; printed Cedar is a local preview, never an approval.
    #[arg(long, value_enum, default_value_t = InitFormat::Cedar)]
    format: InitFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum InitFormat {
    Cedar,
    Json,
}

/// Redacted intent failures without task text, file contents or raw transport errors.
#[derive(Debug, thiserror::Error)]
pub enum IntentFailure {
    /// Task, transport or output size exceeds its budget.
    #[error("intent input exceeds its budget; shorten the task text")]
    Limit,
    /// Local spec input or generation failed; the message names the offending input.
    #[error("{0}")]
    Spec(String),
    /// The candidate had already expired when it was received.
    #[error("{0}")]
    Clarification(&'static str),
    /// A command proposal was rejected locally; no request reached the daemon.
    #[error("invalid command proposal: {0}")]
    Input(&'static str),
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
            Self::Clarification(_) | Self::Input(_) => 2,
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
        let interrupted = Arc::new(AtomicBool::new(false));
        let signal = Interrupt::register(Arc::clone(&interrupted))?;
        let result = match self {
            Self::Propose(options) => options.run(socket, timeout, &interrupted),
            Self::ProposeCommand(options) => options.run(socket, timeout, &interrupted),
            // Init is a pure local computation, so it needs neither the socket nor an
            // interruptible wait.
            Self::Init(options) => options.run(),
        };
        drop(signal);
        check_interrupt(&interrupted)?;
        result
    }
}

impl Init {
    fn run(&self) -> Result<u8, IntentFailure> {
        let spec = self.spec()?;
        let artifact = compile_spec(&spec)
            .map_err(|error| IntentFailure::Spec(render::escape(&error.to_string())))?;
        let mut out = io::stdout().lock();
        match self.format {
            InitFormat::Cedar => {
                writeln!(
                    out,
                    "// LOCAL: generated by intent init; not approved, not effective."
                )?;
                // Spec validation rejects control characters, bidi controls and line or paragraph
                // separators, so the fixed template keeps its line breaks in terminals and exports.
                writeln!(out, "{}", artifact.text)?;
            }
            InitFormat::Json => {
                let value = serde_json::json!({
                    "format_version": INIT_VERSION,
                    "spec": spec,
                    "policy": artifact.text,
                    "schema_sha256": artifact.schema_sha256,
                });
                serde_json::to_writer(&mut out, &value).map_err(|_| IntentFailure::Output)?;
                writeln!(out)?;
            }
        }
        out.flush()?;
        Ok(0)
    }

    fn spec(&self) -> Result<IntentSpec, IntentFailure> {
        let Some(path) = &self.spec_file else {
            return Ok(IntentSpec {
                allow_read: self.allow_read.clone(),
                allow_write: self.allow_write.clone(),
                allow_exec: self.allow_exec.clone(),
                deny_read: self.deny_read.clone(),
                deny_write: self.deny_write.clone(),
                deny_exec: self.deny_exec.clone(),
                allow_network: self.allow_network,
                deny_network: self.deny_network,
                programs: self.program.clone(),
                ..IntentSpec::default()
            });
        };
        let raw = read_spec_source(path).map_err(|error| {
            IntentFailure::Spec(render::escape(&format!(
                "cannot read spec file {path}: {error}"
            )))
        })?;
        if raw.len() > SPEC_FILE_BYTES {
            return Err(IntentFailure::Spec(
                "spec file exceeds its size budget".into(),
            ));
        }
        serde_json::from_str(&raw)
            .map_err(|error| IntentFailure::Spec(render::escape(&format!("invalid spec: {error}"))))
    }
}

fn read_spec_source(path: &str) -> io::Result<String> {
    let mut buffer = String::new();
    let limit = SPEC_FILE_BYTES as u64 + 1;
    if path == "-" {
        io::stdin().lock().take(limit).read_to_string(&mut buffer)?;
    } else {
        std::fs::File::open(path)?
            .take(limit)
            .read_to_string(&mut buffer)?;
    }
    Ok(buffer)
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
