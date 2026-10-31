//! Direct sandbox execution from caller-supplied permissions; no daemon traffic.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;

use asc_policy_types::permissions::os::OsPermissionSet;
use clap::{Args, Subcommand};

const MAX_PERMISSION_BYTES: u64 = 4 * 1024 * 1024;

/// Standalone sandbox commands.
#[derive(Debug, Subcommand)]
pub enum SandboxCommand {
    /// Run a command with explicitly supplied OS permissions.
    Run(SandboxRun),
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
    /// Installs the supplied restrictions and returns the target's exit status.
    ///
    /// # Errors
    /// Rejects malformed permissions, unsupported restrictions and launch failures.
    pub fn run(&self) -> Result<u8, SandboxError> {
        let Self::Run(options) = self;
        let permissions = read_permissions(&options.permissions)?;
        let (program, args) = options
            .command
            .split_first()
            .ok_or(SandboxError::MissingCommand)?;
        let environment = std::env::vars_os()
            .map(|(key, value)| {
                Ok((
                    key.into_string().map_err(|_| SandboxError::Environment)?,
                    value.into_string().map_err(|_| SandboxError::Environment)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, SandboxError>>()?;
        let launch = asc_pep_runtime::prepare_launch(
            program,
            args.to_vec(),
            &std::env::current_dir()?,
            environment,
        )?;
        let result = asc_pep_runtime::execute(launch, permissions)?;
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
