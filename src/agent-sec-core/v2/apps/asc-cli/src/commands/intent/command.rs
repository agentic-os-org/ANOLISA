//! Daemon-only command-to-Cedar proposals: only the shell text and its working directory are
//! sent, and every outcome stays unapproved.

use std::io::{self, IsTerminal as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use asc_daemon_protocol::{
    DaemonRequest, DaemonResponse, ErrorCode, error_code,
    intent::{
        INTENT_COMMAND_REQUEST_VERSION, INTENT_REQUEST_BYTES, INTENT_RESULT_BYTES,
        IntentCommandParams, IntentCommandResult,
    },
    method::INTENT_PROPOSE_COMMAND,
};
use asc_intent::{ActionCategory, CommandInput, PathScope, Permission, PolicySummary};
use clap::Args;

use super::{Format, IntentFailure, check_interrupt, render};

const CWD_HINT: &str = "the working directory must exist and resolve to a UTF-8 absolute path";
const INPUT_HINT: &str = "the command must be non-empty text without NUL bytes and at most 64 KiB, and the working directory must be canonical and at most 4096 bytes";

/// Proposes the Cedar a single hook-observed command needs.
#[derive(Debug, Args)]
pub(crate) struct ProposeCommand {
    /// Shell command exactly as observed; the CLI never runs or interprets it.
    #[arg(long, allow_hyphen_values = true)]
    command: String,
    /// Working directory the command runs in; defaults to the CLI's own directory.
    #[arg(long)]
    cwd: Option<String>,
    /// Output format; JSON is the default because hook callers parse this stream.
    #[arg(long, value_enum, default_value_t = Format::Json)]
    format: Format,
}

impl ProposeCommand {
    pub(crate) fn run(
        &self,
        socket: &Path,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<u8, IntentFailure> {
        check_interrupt(interrupted)?;
        let input = self.input()?;
        let result = propose(socket, timeout, interrupted, &input)?;
        check_interrupt(interrupted)?;
        // The daemon answer is data, not a trusted artifact: rebuild the Cedar locally before
        // anything is rendered.
        validate(&result)?;
        render_result(&result, self.format)?;
        Ok(0)
    }

    /// Resolves the working directory and rejects input the daemon would refuse anyway.
    fn input(&self) -> Result<CommandInput, IntentFailure> {
        let cwd = match &self.cwd {
            // Resolving the directory does not read any command target, and the daemon only
            // accepts a canonical absolute path.
            Some(cwd) => std::fs::canonicalize(cwd).map_err(|_| IntentFailure::Input(CWD_HINT))?,
            None => std::env::current_dir().map_err(|_| IntentFailure::Input(CWD_HINT))?,
        };
        let cwd = cwd.to_str().ok_or(IntentFailure::Input(CWD_HINT))?;
        let input = CommandInput {
            command: self.command.clone(),
            cwd: cwd.to_owned(),
        };
        input
            .validate()
            .map_err(|_| IntentFailure::Input(INPUT_HINT))?;
        Ok(input)
    }
}

fn propose(
    socket: &Path,
    timeout: Duration,
    interrupted: &AtomicBool,
    input: &CommandInput,
) -> Result<IntentCommandResult, IntentFailure> {
    let params = IntentCommandParams {
        format_version: INTENT_COMMAND_REQUEST_VERSION.into(),
        command: input.command.clone(),
        cwd: input.cwd.clone(),
    };
    let request = DaemonRequest {
        method: INTENT_PROPOSE_COMMAND.into(),
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

/// Names the socket and the usual causes when the daemon exposes no command intent service.
fn daemon_hint(code: &ErrorCode, socket: &Path) -> String {
    let socket = socket.display();
    match code.as_str() {
        error_code::UNAVAILABLE => format!(
            "; the intent service on {socket} is disabled or misconfigured, or the model returned no valid policy; check --socket or AGENT_SEC_DAEMON_SOCKET, ASC_INTENT_ENABLED=true and the model settings, then retry"
        ),
        error_code::UNKNOWN_METHOD => format!(
            "; {socket} does not register intent.propose_command, so it is an older daemon; check --socket or AGENT_SEC_DAEMON_SOCKET and restart the daemon built from this source tree"
        ),
        error_code::INVALID_ARGUMENT => "; check the command text, the working directory and the request format version; use CLI and daemon built from the same source".into(),
        error_code::INTERNAL => "; the daemon already retried once and both model answers were rejected by local Cedar validation, so switch ASC_INTENT_MODEL to a model with more reliable Cedar output".into(),
        _ => String::new(),
    }
}

/// Rejects answers that this build cannot reproduce as Cedar or explain as clarification.
fn validate(result: &IntentCommandResult) -> Result<(), IntentFailure> {
    match result {
        IntentCommandResult::Candidate { policy, caveats } => {
            let artifact =
                asc_intent::prepare_policy(policy).map_err(|_| IntentFailure::Response)?;
            // The daemon returns raw model text, so any local rewrite is a protocol violation.
            if artifact.text != *policy || !asc_intent::reasons_valid(caveats) {
                return Err(IntentFailure::Response);
            }
        }
        IntentCommandResult::NeedsClarification { reasons } => {
            if reasons.is_empty() || !asc_intent::reasons_valid(reasons) {
                return Err(IntentFailure::Response);
            }
        }
    }
    Ok(())
}

fn render_result(result: &IntentCommandResult, format: Format) -> Result<(), IntentFailure> {
    let mut out = io::stdout().lock();
    match (format, result) {
        (Format::Json, _) => {
            serde_json::to_writer(&mut out, result).map_err(|_| IntentFailure::Output)?;
            writeln!(out)?;
        }
        (Format::Cedar, IntentCommandResult::Candidate { policy, caveats }) => {
            writeln!(out, "// CANDIDATE: not approved; not effective.")?;
            // Cedar comments may carry ESC/OSC sequences the parser ignores but a terminal
            // executes; pipes and files keep the export byte-for-byte.
            writeln!(out, "{}", render::display(policy, out.is_terminal()))?;
            for caveat in caveats {
                writeln!(out, "// Not covered: {}", render::escape(caveat))?;
            }
        }
        (Format::Cedar, IntentCommandResult::NeedsClarification { reasons }) => {
            // Comments cannot authorize, so a caller that mistakes this stream for Cedar still
            // receives no permit.
            writeln!(out, "// Needs clarification: no Cedar is emitted.")?;
            for reason in reasons {
                writeln!(out, "// {}", render::escape(reason))?;
            }
        }
        (Format::Text, IntentCommandResult::Candidate { policy, caveats }) => {
            writeln!(out, "Candidate generated; not approved or effective.")?;
            if let Some(summary) = asc_intent::summarize(policy) {
                write_summary(&mut out, &summary)?;
            } else {
                // A construct the summary cannot represent would otherwise read as a narrower
                // permission than the policy states, so show the validated text unchanged.
                writeln!(out, "\nValidated Cedar (not approved)")?;
                for line in policy.lines() {
                    writeln!(out, "{}", render::escape(line))?;
                }
            }
            if !caveats.is_empty() {
                writeln!(out, "\nNot covered by this candidate:")?;
                for caveat in caveats {
                    writeln!(out, "  - {}", render::escape(caveat))?;
                }
            }
        }
        (Format::Text, IntentCommandResult::NeedsClarification { reasons }) => {
            writeln!(
                out,
                "Candidate not generated: the command needs clarification before any Cedar exists."
            )?;
            for reason in reasons {
                writeln!(out, "  - {}", render::escape(reason))?;
            }
            writeln!(
                out,
                "\nNo Cedar is emitted, so nothing can be mapped or approved from this run."
            )?;
        }
    }
    out.flush()?;
    Ok(())
}

const SUMMARY_ORDER: [ActionCategory; 5] = [
    ActionCategory::Read,
    ActionCategory::Write,
    ActionCategory::Exec,
    ActionCategory::Spawn,
    ActionCategory::Network,
];

/// Prints one line per action category so missing permissions are as visible as granted ones.
fn write_summary(out: &mut impl io::Write, summary: &PolicySummary) -> io::Result<()> {
    writeln!(out, "\nWhat the candidate would allow (unapproved):")?;
    for category in SUMMARY_ORDER {
        let mut entries = summary
            .permits
            .iter()
            .filter(|permission| permission.action == category)
            .peekable();
        if entries.peek().is_none() {
            writeln!(out, "  {:<8} none", category.label())?;
        }
        for permission in entries {
            writeln!(
                out,
                "  {:<8} {}",
                category.label(),
                render::escape(&describe(permission))
            )?;
        }
    }
    if !summary.forbids.is_empty() {
        writeln!(out, "\nWhat the candidate would deny:")?;
        for permission in &summary.forbids {
            writeln!(
                out,
                "  {:<8} {}",
                permission.action.label(),
                render::escape(&describe(permission))
            )?;
        }
    }
    Ok(())
}

fn describe(permission: &Permission) -> String {
    let mut line = match permission.action {
        ActionCategory::Network => "whole network".to_owned(),
        ActionCategory::Spawn => "any process start".to_owned(),
        _ => permission
            .path
            .as_ref()
            .map_or_else(|| "any file".to_owned(), PathScope::display),
    };
    let mut details = Vec::new();
    if let Some(program) = &permission.program
        && permission.action != ActionCategory::Spawn
    {
        details.push(format!("program {program}"));
    }
    if let Some(operation) = &permission.operation {
        details.push(format!("operation {operation}"));
    }
    if let Some(cwd) = &permission.cwd {
        details.push(format!("cwd {cwd}"));
    }
    if !details.is_empty() {
        line = format!("{line} ({})", details.join(", "));
    }
    line
}
