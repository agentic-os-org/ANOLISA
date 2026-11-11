//! Cosh keeps approval and foreground execution; the daemon holds the immutable handoff.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use asc_daemon_protocol::authorization::{
    AgentPrincipal, ClaimPhase, ClaimResponse, Decision, DecisionClaim, DecisionVerdict,
    ExecutionSpec,
};
use asc_pep_runtime::{
    ApprovalHandler, DaemonAuthorizationClient, PepError, PepRuntime, SandboxBackend,
    prepare_launch,
};
use clap::Subcommand;
use serde::Deserialize;

use super::{SandboxError, exit_status, read_permissions};

mod message;

/// Private entry points called by the packaged Cosh hook and its rewritten command.
#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// Authorize the Hook JSON from stdin without executing the target.
    Prepare {
        /// OS permission JSON or Cedar request source, including runtime dependencies.
        #[arg(long)]
        permissions: PathBuf,
    },
    /// Consume a handoff after Cosh has approved the rewritten command.
    Execute {
        /// Opaque reference to the daemon-held invocation.
        decision_id: String,
    },
    /// Close an individual handoff after the host has finished the tool.
    Finish {
        /// Opaque reference from the rewritten tool input.
        decision_id: String,
    },
}

#[derive(Deserialize)]
struct HookInput {
    session_id: String,
    run_id: Option<String>,
    cwd: PathBuf,
    tool_input: ToolInput,
}

#[derive(Deserialize)]
struct ToolInput {
    command: String,
}

struct HostApproved;
impl ApprovalHandler for HostApproved {
    fn ask(&mut self, _: &Decision) -> Result<bool, PepError> {
        // This entry point is the command Cosh runs only after its native approval gate.
        Ok(true)
    }
}

impl HookCommand {
    pub(super) fn run(&self, socket: &Path, timeout: Duration) -> Result<u8, SandboxError> {
        let client = DaemonAuthorizationClient {
            socket: socket.to_owned(),
            timeout,
        };
        match self {
            Self::Prepare { permissions } => prepare(permissions, &client),
            Self::Execute { decision_id } => {
                let accepted = claim(&client, decision_id, ClaimPhase::Execute, None, None)?
                    .authorization
                    .ok_or_else(|| {
                        SandboxError::Execution("authorization already consumed".into())
                    })?;
                let mut runtime = PepRuntime::new(SandboxBackend::installed()?);
                let outcome = runtime.resume(accepted, &client, Some(&mut HostApproved))?;
                if let Some(error) = outcome.receipt_error {
                    return Err(SandboxError::Execution(format!(
                        "execution completed but receipt failed; do not rerun: {error}"
                    )));
                }
                exit_status(outcome.result)
            }
            Self::Finish { decision_id } => {
                claim(&client, decision_id, ClaimPhase::Abandon, None, None)?;
                Ok(0)
            }
        }
    }
}

fn claim(
    client: &DaemonAuthorizationClient,
    id: &str,
    phase: ClaimPhase,
    execution: Option<ExecutionSpec>,
    run_id: Option<String>,
) -> Result<ClaimResponse, SandboxError> {
    let response = asc_daemon_client::authorization_claim(
        &client.socket,
        &DecisionClaim {
            decision_id: id.into(),
            phase,
            run_id,
            execution,
        },
        client.timeout,
    )
    .map_err(|error| SandboxError::Execution(error.to_string()))?;
    match response {
        asc_daemon_protocol::DaemonResponse::Success(response)
            if response.result.decision_id == id =>
        {
            Ok(response.result)
        }
        asc_daemon_protocol::DaemonResponse::Success(_) => Err(SandboxError::Execution(
            "handoff response ID mismatch".into(),
        )),
        asc_daemon_protocol::DaemonResponse::Error(response) => {
            Err(SandboxError::Execution(response.error.message().into()))
        }
    }
}

fn prepare(permissions: &Path, client: &DaemonAuthorizationClient) -> Result<u8, SandboxError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(super::MAX_PERMISSION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > super::MAX_PERMISSION_BYTES {
        return Err(SandboxError::TooLarge);
    }
    let input: HookInput = serde_json::from_slice(&bytes)?;
    if input.session_id.is_empty()
        || !input.cwd.is_absolute()
        || input.tool_input.command.trim().is_empty()
    {
        return Err(SandboxError::Execution(
            "hook requires session_id, absolute cwd and command".into(),
        ));
    }
    if let Some(run) = &input.run_id {
        claim(client, "", ClaimPhase::FinishRun, None, Some(run.clone()))?;
    }
    let launch = prepare_launch(
        "/bin/bash",
        vec!["-c".into(), input.tool_input.command],
        &input.cwd,
        super::target_environment()?,
    )?;
    let mut runtime = PepRuntime::new(SandboxBackend::installed()?);
    let (request, decision) = runtime.authorize(
        &AgentPrincipal {
            agent_type: Some("cosh".into()),
        },
        launch,
        &read_permissions(permissions)?,
        client,
    )?;
    let output = if matches!(
        decision.verdict,
        DecisionVerdict::Deny | DecisionVerdict::Defer
    ) {
        let message = message::format(&request.request, &decision, &[]);
        let outcome = runtime.apply(&request, decision, client, None)?;
        if let Some(error) = outcome.receipt_error {
            return Err(SandboxError::Execution(format!(
                "denial receipt failed: {error}"
            )));
        }
        serde_json::json!({"decision":"block", "reason":message, "systemMessage":message})
    } else {
        let execution = match runtime.prepare_execution(&request, &decision) {
            Ok(spec) => spec,
            Err(error) => {
                PepRuntime::report_failure(&decision, client, error.to_string())?;
                return Err(error.into());
            }
        };
        let message = message::format(&request.request, &decision, &execution.path_facts);
        let executable = std::env::current_exe()?
            .into_os_string()
            .into_string()
            .map_err(|_| SandboxError::Environment)?;
        let socket = client.socket.to_str().ok_or(SandboxError::Environment)?;
        let command = [
            executable.as_str(),
            "--socket",
            socket,
            "sandbox",
            "hook",
            "execute",
            &decision.decision_id,
        ]
        .into_iter()
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ");
        claim(
            client,
            &decision.decision_id,
            ClaimPhase::Accept,
            Some(execution),
            input.run_id,
        )?;
        serde_json::json!({
            "decision": if decision.verdict == DecisionVerdict::StepUp { "ask" } else { "allow" },
            "reason":message, "systemMessage":message,
            "hookSpecificOutput":{"tool_input":{"command":command}}
        })
    };
    serde_json::to_writer(std::io::stdout().lock(), &output)?;
    writeln!(std::io::stdout().lock())?;
    Ok(0)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
