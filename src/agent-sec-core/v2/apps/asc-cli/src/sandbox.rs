//! Direct permission-based execution and the internal Cosh authorization handoff.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;

use asc_os_permission_types::OsPermissionSet;
use clap::{Args, Subcommand};

pub mod hook;

const MAX_PERMISSION_BYTES: u64 = 4 * 1024 * 1024;

/// Sandbox execution entry points.
#[derive(Debug, Subcommand)]
pub enum SandboxCommand {
    /// Run a command with explicitly supplied OS permissions.
    Run(SandboxRun),
    /// Internal Cosh authorization and execution handoff.
    #[command(subcommand, hide = true)]
    Hook(hook::HookCommand),
}

/// Inputs to one direct sandbox launch.
#[derive(Debug, Args)]
pub struct SandboxRun {
    /// JSON file containing an OS permission set.
    #[arg(long)]
    pub permissions: PathBuf,
    /// Target executable and arguments, following `--`.
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}

impl SandboxCommand {
    /// Only the explicit host integration uses the daemon.
    pub const fn requires_daemon(&self) -> bool {
        matches!(self, Self::Hook(_))
    }
    /// Installs the supplied restrictions and returns the target's exit status.
    ///
    /// # Errors
    /// Rejects malformed permissions, unsupported restrictions and launch failures.
    pub fn run(
        &self,
        socket: Option<&std::path::Path>,
        timeout: std::time::Duration,
    ) -> Result<u8, SandboxError> {
        let options = match self {
            Self::Run(options) => options,
            Self::Hook(command) => {
                return command.run(
                    socket.ok_or_else(|| {
                        SandboxError::Execution("authorization socket missing".into())
                    })?,
                    timeout,
                );
            }
        };
        let permissions = read_permissions(&options.permissions)?;
        let (program, args) = options
            .command
            .split_first()
            .ok_or(SandboxError::MissingCommand)?;
        let launch = asc_pep_runtime::prepare_launch(
            program,
            args.to_vec(),
            &std::env::current_dir()?,
            target_environment()?,
        )?;
        let result = asc_pep_runtime::execute(launch, permissions)?;
        exit_status(result)
    }
}

fn target_environment() -> Result<BTreeMap<String, String>, SandboxError> {
    std::env::vars_os()
        .map(|(key, value)| {
            Ok((
                key.into_string().map_err(|_| SandboxError::Environment)?,
                value.into_string().map_err(|_| SandboxError::Environment)?,
            ))
        })
        .collect()
}

fn exit_status(
    result: asc_daemon_protocol::authorization::ExecutionResult,
) -> Result<u8, SandboxError> {
    if let Some(error) = result.error {
        return Err(SandboxError::Execution(error));
    }
    if let Some(signal) = result.cancel_signal {
        return Ok(u8::try_from(128_i32.saturating_add(signal)).unwrap_or(255));
    }
    if !result.installed || !result.started {
        return Err(SandboxError::Execution(
            "target was not started in a sandbox".into(),
        ));
    }
    if let Some(signal) = result.signal {
        return Ok(u8::try_from(128_i32.saturating_add(signal)).unwrap_or(255));
    }
    result
        .exit_code
        .and_then(|code| u8::try_from(code).ok())
        .ok_or_else(|| SandboxError::Execution("target did not return an exit status".into()))
}

fn read_permissions(path: &std::path::Path) -> Result<OsPermissionSet, SandboxError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_PERMISSION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PERMISSION_BYTES {
        return Err(SandboxError::TooLarge);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Direct sandbox input and execution failures.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The required target was absent.
    #[error("sandbox run requires a command after --")]
    MissingCommand,
    /// Permission input is bounded independently of its parsed structure.
    #[error("sandbox permissions exceed the 4 MiB input limit")]
    TooLarge,
    /// JSON contracts cannot preserve non-Unicode environment values.
    #[error("sandbox environment must be valid UTF-8")]
    Environment,
    /// Local file or working directory could not be read.
    #[error("cannot read sandbox input: {0}")]
    Io(#[from] std::io::Error),
    /// The permission object did not match the supported schema.
    #[error("invalid sandbox permissions: {0}")]
    Json(#[from] serde_json::Error),
    /// Permissions or runtime prerequisites could not be applied.
    #[error(transparent)]
    Runtime(#[from] asc_pep_runtime::PepError),
    /// The backend reported a failed installation or launch.
    #[error("sandbox execution failed: {0}")]
    Execution(String),
}
