use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::cli::{CliArgs, ExecutionProfile, PublicExec, PublicFileTools, PublicOutput};
use crate::config::CoreConfig;
use crate::core::public::PublicHeadlessRuntime;
use crate::core::{AgentTurnOutcome, CoshCore};
use crate::extension::{RuntimeGeneration, RuntimeSnapshot};
use crate::tool::{SessionWorkspace, ToolRegistry, WorkspaceStartupPolicy};

const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
mod mock;
mod output;

#[derive(Default)]
struct Cancellation {
    #[cfg(unix)]
    signals: Option<(tokio::signal::unix::Signal, tokio::signal::unix::Signal)>,
}

impl Cancellation {
    fn register() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Self {
                signals: Some((
                    signal(SignalKind::interrupt())?,
                    signal(SignalKind::terminate())?,
                )),
            })
        }
        #[cfg(not(unix))]
        Ok(Self::default())
    }

    async fn wait(&mut self) {
        #[cfg(unix)]
        if let Some((interrupt, terminate)) = &mut self.signals {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
        std::future::pending::<()>().await;
    }
}

#[derive(Serialize)]
struct PublicResult {
    schema_version: u32,
    status: &'static str,
    error_code: Option<&'static str>,
    answer: String,
}

pub(crate) async fn run(args: &CliArgs) -> i32 {
    let output = args.output.unwrap_or_default();
    let started = tokio::time::Instant::now();
    let mut cancellation = Cancellation::default();
    let timeout = validate_args(args).and_then(|()| {
        parse_timeout(
            args.timeout.as_deref(),
            args.file_tools == Some(PublicFileTools::Write) || args.exec == Some(PublicExec::Host),
        )
    });
    let timeout = match timeout {
        Ok(timeout) => timeout,
        Err(error) => {
            return emit_failure(output, "invalid_input", &error, 2, &mut cancellation).await
        }
    };
    let deadline = started + timeout;
    let result = match Cancellation::register() {
        Ok(registered) => {
            cancellation = registered;
            run_task(args, deadline, &mut cancellation).await
        }
        Err(error) => Err(cancellation_registration_failure(error)),
    };
    match result {
        Ok(answer) => emit_success(output, answer, deadline, &mut cancellation).await,
        Err(failure) => {
            emit_failure(
                output,
                failure.code,
                &failure.message,
                failure.exit_code,
                &mut cancellation,
            )
            .await
        }
    }
}

fn cancellation_registration_failure(error: std::io::Error) -> Failure {
    Failure::new(
        "runtime_unavailable",
        format!("cannot register task cancellation: {error}"),
        1,
    )
}

async fn run_task(
    args: &CliArgs,
    deadline: tokio::time::Instant,
    cancellation: &mut Cancellation,
) -> Result<String, Failure> {
    let file_tools = args.file_tools.unwrap_or_default();
    let prompt = tokio::select! {
        result = read_prompt(args, deadline) => result,
        _ = cancellation.wait() => Err(Failure::new("cancelled", "task cancelled", 1)),
    }?;
    let loaded = CoreConfig::load_public_headless()
        .map_err(|error| Failure::new("policy_unavailable", error, 1))?;
    if args.exec == Some(PublicExec::Host)
        && (!loaded.allow_host_exec || sandbox_all_shell_requested())
    {
        return Err(Failure::new(
            "execution_forbidden",
            "host execution is forbidden by deployment policy",
            1,
        ));
    }
    cosh_platform::audit::config::load_audit_settings(None)
        .map_err(|error| Failure::new("policy_unavailable", error.to_string(), 1))?;
    let public_policies = cosh_platform::audit::policy::LoadedPolicy::load_public_headless()
        .map_err(|error| Failure::new("policy_unavailable", error.to_string(), 1))?;
    let mut config = loaded.config;
    crate::logging::init_logging(&config.logging.effective_level(false));
    config.agent.max_turns = args.max_turns.unwrap_or(12);
    if crate::needs_auth(&config)
        || config.resolve_provider().auth_source.as_deref() == Some("ecs_identity")
    {
        return Err(Failure::new(
            "auth_required",
            "the selected provider needs non-interactive credentials",
            1,
        ));
    }
    let project_root =
        workspace_root(args).map_err(|error| Failure::new("workspace_unavailable", error, 1))?;
    let workspace = if file_tools == PublicFileTools::None && args.exec.is_none() {
        SessionWorkspace::new(&project_root)
    } else {
        let workspace_policy = if file_tools == PublicFileTools::None {
            WorkspaceStartupPolicy::AllowUnavailableFileTools
        } else {
            WorkspaceStartupPolicy::RequireConfinement
        };
        SessionWorkspace::try_new(&project_root, workspace_policy)
            .map_err(|error| Failure::new("workspace_unavailable", error, 1))?
    };
    let project_root = workspace.root().to_path_buf();
    let provider =
        mock::create_provider(&config).unwrap_or_else(|| crate::create_provider(&config));
    let mut tools = ToolRegistry::public_headless_files(file_tools);
    if args.exec == Some(PublicExec::Host) {
        tools.register(Box::new(crate::tool::shell::RunCommandTool));
    }
    let snapshot = RuntimeSnapshot::bootstrap(
        RuntimeGeneration::healthy(0, "public-headless-v1"),
        Arc::new(tools),
    );
    let session_id = uuid::Uuid::new_v4().to_string();
    let public_runtime = PublicHeadlessRuntime::new(public_policies);
    let extra_params = config.resolve_provider().extra_params;
    let mut engine = CoshCore::new_public_headless(
        config,
        provider,
        snapshot,
        session_id,
        project_root,
        workspace,
        public_runtime,
    );
    engine.extra_params = extra_params;
    let mut reader = BufReader::new(&b""[..]).lines();
    let mut protocol_sink = std::io::sink();
    let turn = tokio::select! {
        result = tokio::time::timeout_at(deadline, engine.handle_user_message(&prompt, &mut reader, &mut protocol_sink)) => Some(result),
        _ = cancellation.wait() => None,
    };
    let failure = match turn {
        Some(Ok(Ok(AgentTurnOutcome::Completed))) => {
            if let Some(code) = engine.public_failure_code() {
                engine.shutdown_extension_runtime().await;
                return Err(Failure::new(code, "public headless task failed", 1));
            }
            let answer = engine
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "assistant")
                .map(|message| message.content.as_text())
                .unwrap_or_default();
            engine.shutdown_extension_runtime().await;
            return Ok(answer);
        }
        Some(Ok(Ok(AgentTurnOutcome::MaxTurns { limit }))) => Failure::new(
            "max_turns",
            format!("agent exceeded max turns ({limit})"),
            1,
        ),
        Some(Ok(Err(error))) => {
            let code =
                engine
                    .public_failure_code()
                    .unwrap_or(if crate::auth::is_auth_error(&error) {
                        "auth_required"
                    } else {
                        "provider_error"
                    });
            Failure::new(code, error, 1)
        }
        Some(Err(_)) => {
            engine.provider.cancel();
            timeout_failure(engine.public_cleanup_failed())
        }
        None => {
            engine.provider.cancel();
            Failure::new(
                if engine.public_cleanup_failed() {
                    "cleanup_failed"
                } else {
                    "cancelled"
                },
                "task cancelled",
                1,
            )
        }
    };
    engine.shutdown_extension_runtime().await;
    Err(failure)
}

fn validate_args(args: &CliArgs) -> Result<(), String> {
    if !args.headless || args.execution_profile != ExecutionProfile::PublicHeadlessV1 {
        return Err("invalid public headless launch profile".to_string());
    }
    if args.command.is_some()
        || args.model.is_some()
        || args.approval_mode.is_some()
        || args.allowed_tools.is_some()
        || args.tools.is_some()
        || args.bare
        || args.resume.is_some()
        || args.compact
        || args.session_control
        || args.cosh_shell_transport
        || args.registry
        || args.enable_shell_evidence_tool
        || args.output_format.is_some()
        || args.input_format.is_some()
        || args.include_partial_messages
        || args.verbose
    {
        return Err("legacy Core options are not accepted by public headless mode".to_string());
    }
    if args.prompt.is_none() {
        return Err("a prompt or - is required".to_string());
    }
    if args
        .file_tools
        .is_some_and(|mode| mode != PublicFileTools::None)
        && args
            .workspace
            .as_deref()
            .is_none_or(|workspace| workspace.is_empty())
    {
        return Err("--file-tools=read|write requires --workspace PATH".to_string());
    }
    if args.exec == Some(PublicExec::Host)
        && args
            .workspace
            .as_deref()
            .is_none_or(|workspace| workspace.is_empty())
    {
        return Err("--exec=host requires --workspace PATH".to_string());
    }
    Ok(())
}

fn sandbox_all_shell_requested() -> bool {
    std::env::var("AGENT_SEC_COSH_SANDBOX_ALL_SHELL")
        .is_ok_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"))
}

fn parse_timeout(value: Option<&str>, writes_files: bool) -> Result<Duration, String> {
    let Some(value) = value else {
        return Ok(Duration::from_secs(if writes_files { 300 } else { 120 }));
    };
    let (number, multiplier) = match value.strip_suffix(['s', 'm']) {
        Some(number) if value.ends_with('m') => (number, 60),
        Some(number) => (number, 1),
        None => (value, 1),
    };
    let amount = number
        .parse::<u64>()
        .ok()
        .and_then(|amount| amount.checked_mul(multiplier))
        .filter(|seconds| (1..=1800).contains(seconds))
        .ok_or_else(|| {
            "timeout must be a positive number of seconds or minutes, up to 30m".to_string()
        })?;
    Ok(Duration::from_secs(amount))
}

struct Failure {
    code: &'static str,
    message: String,
    exit_code: i32,
}

fn timeout_failure(cleanup_failed: bool) -> Failure {
    if cleanup_failed {
        Failure::new(
            "cleanup_failed",
            "a supervised command group could not be cleaned up after timeout",
            1,
        )
    } else {
        Failure::new("timeout", "headless task timed out", 1)
    }
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code,
        }
    }
}

async fn read_prompt(args: &CliArgs, deadline: tokio::time::Instant) -> Result<String, Failure> {
    let prompt = args.prompt.as_deref().unwrap_or_default();
    let input = if prompt == "-" {
        if std::io::stdin().is_terminal() {
            return Err(Failure::new(
                "invalid_input",
                "- requires non-terminal stdin",
                2,
            ));
        }
        let mut bytes = Vec::new();
        let mut input = tokio::io::stdin().take((MAX_INPUT_BYTES + 1) as u64);
        match tokio::time::timeout_at(deadline, input.read_to_end(&mut bytes)).await {
            Ok(Ok(_)) => bytes,
            Ok(Err(error)) => {
                return Err(Failure::new(
                    "invalid_input",
                    format!("failed to read stdin: {error}"),
                    2,
                ))
            }
            Err(_) => return Err(Failure::new("timeout", "timed out while reading stdin", 1)),
        }
    } else {
        prompt.as_bytes().to_vec()
    };
    if input.len() > MAX_INPUT_BYTES {
        return Err(Failure::new(
            "input_too_large",
            "prompt exceeds the 1 MiB input limit",
            2,
        ));
    }
    let prompt = String::from_utf8(input)
        .map_err(|_| Failure::new("invalid_input", "prompt is not valid UTF-8", 2))?;
    if prompt.trim().is_empty() {
        return Err(Failure::new("invalid_input", "prompt must not be empty", 2));
    }
    Ok(prompt)
}

fn workspace_root(args: &CliArgs) -> Result<std::path::PathBuf, String> {
    let root = args.workspace_path();
    let canonical = std::fs::canonicalize(&root)
        .map_err(|error| format!("workspace {} is unavailable: {error}", root.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "workspace {} is not a directory",
            canonical.display()
        ));
    }
    Ok(canonical)
}

async fn emit_success(
    output: PublicOutput,
    answer: String,
    deadline: tokio::time::Instant,
    cancellation: &mut Cancellation,
) -> i32 {
    if answer.len() > MAX_OUTPUT_BYTES {
        return emit_failure(
            output,
            "output_limit",
            "final answer exceeded 1 MiB",
            1,
            cancellation,
        )
        .await;
    }
    emit_result(
        output,
        PublicResult {
            schema_version: 1,
            status: "completed",
            error_code: None,
            answer,
        },
        deadline,
        cancellation,
    )
    .await
}

async fn emit_failure(
    output: PublicOutput,
    code: &'static str,
    message: &str,
    exit_code: i32,
    cancellation: &mut Cancellation,
) -> i32 {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    if output == PublicOutput::Text {
        let bytes = format!("cosh: {code}: {message}\n");
        let _ = output::write(std::io::stderr(), bytes.as_bytes(), deadline, cancellation).await;
        return exit_code;
    }
    let result = PublicResult {
        schema_version: 1,
        status: "failed",
        error_code: Some(code),
        answer: message.to_string(),
    };
    let _ = emit_result(output, result, deadline, cancellation).await;
    exit_code
}

async fn emit_result(
    output: PublicOutput,
    result: PublicResult,
    deadline: tokio::time::Instant,
    cancellation: &mut Cancellation,
) -> i32 {
    let mut exit_code = 0;
    let mut bytes = if output == PublicOutput::Json {
        match serde_json::to_vec(&result) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("cosh: failed to serialize result: {error}");
                return 1;
            }
        }
    } else {
        result.answer.into_bytes()
    };
    if bytes.len() > MAX_OUTPUT_BYTES && output == PublicOutput::Json {
        bytes = match serde_json::to_vec(&PublicResult {
            schema_version: 1,
            status: "failed",
            error_code: Some("output_limit"),
            answer: "final result exceeded 1 MiB".to_string(),
        }) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("cosh: failed to serialize output-limit result: {error}");
                return 1;
            }
        };
        exit_code = 1;
    }
    if bytes.len() > MAX_OUTPUT_BYTES {
        eprintln!("cosh: output_limit: result exceeded 1 MiB");
        return 1;
    }
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    if output::write(std::io::stdout(), &bytes, deadline, cancellation)
        .await
        .is_err()
    {
        return 1;
    }
    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_registration_failure_is_not_a_policy_failure() {
        let failure =
            cancellation_registration_failure(std::io::ErrorKind::PermissionDenied.into());
        assert_eq!(failure.code, "runtime_unavailable");
        assert_eq!(failure.exit_code, 1);
        assert!(failure.message.contains("task cancellation"));
    }

    #[test]
    fn timeout_projects_cleanup_failure_without_hiding_normal_timeouts() {
        assert_eq!(timeout_failure(true).code, "cleanup_failed");
        assert_eq!(timeout_failure(false).code, "timeout");
    }
}
