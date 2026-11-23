//! Direct permission-based execution and the internal Cosh authorization handoff.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;

use asc_daemon_protocol::authorization::LaunchSpec;
use asc_os_permission_types::OsPermissionSet;
use asc_pep_runtime::PermissionInput;
use asc_sandbox_docker::{DockerBackend, DockerConfig};
use clap::{Args, Subcommand, ValueEnum};

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

/// Enforcement backend for direct sandbox execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendKind {
    /// Host namespace helper installed alongside the CLI.
    Linux,
    /// One container per invocation from an operator-pinned image.
    Docker,
}

/// Inputs to one direct sandbox launch.
#[derive(Debug, Args)]
pub struct SandboxRun {
    /// OS permission JSON or Cedar policy file.
    #[arg(long)]
    pub permissions: PathBuf,
    /// Enforcement backend; docker requires an operator-pinned --image.
    #[arg(long, value_enum, default_value = "linux")]
    pub backend: BackendKind,
    /// Operator-pinned container image, preferably by digest (docker backend).
    #[arg(long, required_if_eq("backend", "docker"))]
    pub image: Option<String>,
    /// Trusted docker CLI absolute path (docker backend).
    #[arg(long, default_value = "/usr/bin/docker")]
    pub docker_cli: PathBuf,
    /// Trusted local Docker Unix endpoint; personal contexts are not used.
    #[arg(long, default_value = "unix:///var/run/docker.sock")]
    pub docker_host: String,
    /// Container memory limit in docker syntax, such as 512m (docker backend).
    #[arg(long)]
    pub memory: Option<String>,
    /// Container CPU quota in docker syntax, such as 2.0 (docker backend).
    #[arg(long)]
    pub cpus: Option<String>,
    /// Container process limit; a default applies when unset (docker backend).
    #[arg(long)]
    pub pids_limit: Option<u32>,
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
        let (program, args) = options
            .command
            .split_first()
            .ok_or(SandboxError::MissingCommand)?;
        let result = match options.backend {
            BackendKind::Linux => {
                let permissions = read_permissions(&options.permissions)?;
                let launch = asc_pep_runtime::prepare_launch(
                    program,
                    args.to_vec(),
                    &std::env::current_dir()?,
                    target_environment()?,
                )?;
                asc_pep_runtime::LinuxSandboxBackend::installed()?
                    .execute_with_provider(launch, &permissions)?
            }
            BackendKind::Docker => {
                let permissions = read_docker_permissions(&options.permissions)?;
                docker_execute(options, program, args, permissions)?
            }
        };
        exit_status(result)
    }
}

/// Runs the target inside one container; the program path is image-relative
/// and must be absolute, and the working directory must be an allowed mount.
fn docker_execute(
    options: &SandboxRun,
    program: &str,
    args: &[String],
    permissions: OsPermissionSet,
) -> Result<asc_daemon_protocol::authorization::ExecutionResult, SandboxError> {
    if !program.starts_with('/') {
        return Err(SandboxError::Execution(
            "docker backend requires an absolute program path inside the image".into(),
        ));
    }
    let cwd = std::env::current_dir()?;
    let launch = LaunchSpec {
        program: program.into(),
        resolved_program: program.into(),
        args: args.to_vec(),
        cwd: cwd.to_str().ok_or(SandboxError::Environment)?.to_owned(),
        env: target_environment()?,
    };
    let backend = DockerBackend::new(DockerConfig {
        cli: options.docker_cli.clone(),
        host: options.docker_host.clone(),
        guard: None,
        image: options.image.clone().unwrap_or_default(),
        memory: options.memory.clone(),
        cpus: options.cpus.clone(),
        pids_limit: options.pids_limit,
    })?;
    Ok(backend.execute(launch, permissions)?)
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

fn read_permissions(path: &std::path::Path) -> Result<PermissionInput, SandboxError> {
    Ok(PermissionInput::parse(&read_permission_text(path)?)?)
}

fn read_docker_permissions(path: &std::path::Path) -> Result<OsPermissionSet, SandboxError> {
    let text = read_permission_text(path)?;
    if !text.trim_start().starts_with('{') {
        return Err(SandboxError::Execution(
            "docker backend requires OS permission JSON; Cedar inputs require the linux backend"
                .into(),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&text)?;
    if value.get("language").is_some() {
        return Err(SandboxError::Execution(
            "docker backend requires OS permission JSON; Cedar inputs require the linux backend"
                .into(),
        ));
    }
    Ok(serde_json::from_value(value)?)
}

fn read_permission_text(path: &std::path::Path) -> Result<String, SandboxError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_PERMISSION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PERMISSION_BYTES {
        return Err(SandboxError::TooLarge);
    }
    Ok(std::str::from_utf8(&bytes)?.to_owned())
}

/// Direct sandbox input and execution failures.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// A Cedar or JSON source could not become a permission request.
    #[error(transparent)]
    PermissionInput(#[from] asc_pep_runtime::permissions::PermissionInputError),
    /// Policy source text must have a lossless UTF-8 representation.
    #[error("sandbox permission input must be valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
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
    /// The docker backend rejected the placement or its configuration.
    #[error(transparent)]
    Docker(#[from] asc_sandbox_docker::DockerError),
    /// The backend reported a failed installation or launch.
    #[error("sandbox execution failed: {0}")]
    Execution(String),
}
