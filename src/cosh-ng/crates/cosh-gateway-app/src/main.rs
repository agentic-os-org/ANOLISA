#![forbid(unsafe_code)]
//! Installed local entrypoint for ACP v1 and the local Task control plane.
//!
//! Command modules own ACP, Task, administration, and bounded input behavior;
//! this entrypoint retains only shared presentation and exit-code contracts.

use std::fs::File;
use std::io::{self, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use cosh_gateway::daemon::{
    AppendTaskInput, CancelTask, GatewayDaemon, GatewayDaemonConfig, GatewayResult,
    InspectTaskSnapshot, LaunchReadiness, LocalGatewayClient, ResolveApproval, RetryTask,
    SubmitLaunch, SwitchTaskSnapshot, TaskLaunchCatalog,
};
use cosh_gateway::permission::{
    CancelPermissionPresenter, FilePermissionEvidenceSink, OncePermissionProxy,
    PermissionEvidenceContext, PermissionPresenter, TextPermissionPresenter,
};
use cosh_gateway::runtime::{
    AcpRuntimeProfileId, AcpRuntimeProfileRequest, AcpRuntimeProfileResolver, AcpSessionDriver,
    AcpSessionDriverConfig, AcpSessionEvent, AcpSessionObservation, AcpSessionTerminalKind,
    AcpV1ClientConfig, AcpV1Observation, AcpV1PermissionDecision, AcpV1PermissionOptionKind,
    AcpV1RequestId, AcpV1StopReason, InstalledAcpRuntimePortFactory,
    InstalledBrokeredCoreRuntimePortFactory, LinuxSystemdContainmentVerifier, LocalOsActorResolver,
    ScheduledAgentRuntimeFactory, TrustedWorkspaceResolver, GATEWAY_BROKERED_CORE_RUNTIME_PROFILE,
    GATEWAY_CHECKPOINT_CORE_RUNTIME_PROFILE, GATEWAY_WORKSPACE_WRITE_CORE_RUNTIME_PROFILE,
};
use cosh_gateway::storage::{inspect_task_store, SqliteTaskStore, StoreInspectionOutcome};
use cosh_gateway_contracts::{
    capability::ApprovalDecision,
    common::{BoundedName, BoundedOpaque, BoundedText, Digest, IdempotencyKey, RuntimeSelector},
    ids::{
        ApprovalId, CheckpointId, InputRequestId, InstallationId, RequestId, RunId,
        RuntimeInstanceId, TaskId,
    },
    profile::GatewayCapabilityProfile,
    runtime::{RuntimeInputResponse, RuntimeInputSelections},
    task::{ApprovalPolicy, CheckpointPolicy, TaskLaunchSpecV1, TaskRuntime},
};
use serde_json::{json, Value};
use thiserror::Error;

mod checkpoint;
mod path;
#[cfg(target_os = "linux")]
mod web;

#[path = "cosh_gateway/acp_command.rs"]
mod acp_command;
#[path = "cosh_gateway/control.rs"]
mod control;
#[path = "cosh_gateway/input.rs"]
mod input;
#[path = "cosh_gateway/serve.rs"]
mod serve;

#[cfg(test)]
use acp_command::with_observation_sequence;
use acp_command::{doctor, install_interrupt_handler, run};
use control::{admin, task};
use input::{read_intent, read_prompt, terminal_safe};
use path::{daemon_database_path, daemon_socket_path, require_absolute};
use serve::{serve, ServeArgs};
#[cfg(target_os = "linux")]
use web::{web, WebArgs};

const MAX_ACP_FRAME_BYTES: usize = 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 256 * 1024;
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const EVENT_DEADLINE: Duration = Duration::from_secs(15);
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

const EXIT_INPUT: u8 = 10;
const EXIT_PROFILE: u8 = 11;
const EXIT_RUNTIME: u8 = 12;
const EXIT_AGENT: u8 = 13;
const EXIT_STORE_INSPECTION: u8 = 14;
const EXIT_CANCELLED: u8 = 130;

#[derive(Debug, Parser)]
#[command(
    name = "cosh-gateway",
    version,
    about = "Run ACP interoperability and brokered local Gateway tasks through COSH"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify an installed adapter through initialize and session/new.
    Doctor(ProfileArgs),
    /// Run one text prompt read from stdin or an explicit file.
    Run(RunArgs),
    /// Run the brokered local Task control daemon.
    Serve(ServeArgs),
    /// Submit, inspect, follow, or cancel durable Tasks through the daemon.
    Task(TaskArgs),
    /// Run local read-only Gateway administration commands.
    Admin(AdminArgs),
    /// Open a loopback-only browser view for brokered-only Tasks (Linux only).
    #[cfg(target_os = "linux")]
    Web(WebArgs),
}

#[derive(Debug, Clone, Args)]
struct AdminArgs {
    /// Presentation format for bounded local diagnostics.
    #[arg(long, value_enum, default_value_t = Output::Human)]
    output: Output,
    #[command(subcommand)]
    command: AdminCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum AdminCommand {
    /// Inspect an existing Task store without migration or repair.
    Inspect(AdminInspectArgs),
}

#[derive(Debug, Clone, Args)]
struct AdminInspectArgs {
    /// Absolute path to an existing private Gateway SQLite database.
    #[arg(long, value_name = "PATH")]
    database: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct TaskArgs {
    /// Absolute Unix socket path; defaults from COSH_GATEWAY_SOCKET or the user runtime directory.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    /// Presentation format for bounded daemon responses.
    #[arg(long, value_enum, default_value_t = Output::Human)]
    output: Output,
    #[command(subcommand)]
    command: TaskCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum TaskCommand {
    /// Report the sealed Runtime, checkpoint, and workspace launch catalog.
    Capabilities,
    /// Create one durable Task from stdin or a regular file.
    Submit(TaskSubmitArgs),
    /// List recent durable Tasks owned by the current local user.
    List(TaskListArgs),
    /// Read the current durable Task projection.
    Get(TaskIdArgs),
    /// Read a bounded page of durable Task events.
    Events(TaskEventsArgs),
    /// Request cancellation of the active Task Run.
    Cancel(TaskCancelArgs),
    /// Resolve a pending Runtime or brokered approval.
    ResolveApproval(TaskResolveApprovalArgs),
    /// Append one exact response to a pending Runtime question.
    Append(TaskAppendArgs),
    /// Queue a replacement for one exact suspended Run.
    Retry(TaskRetryArgs),
    /// Inspect or switch checkpoints owned by a managed Task.
    Snapshot(TaskSnapshotArgs),
}

#[derive(Debug, Clone, Args)]
struct TaskSnapshotArgs {
    #[command(subcommand)]
    command: TaskSnapshotCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum TaskSnapshotCommand {
    /// List proven-created checkpoints owned by one Task.
    List(TaskIdArgs),
    /// Preview changes required to switch to one exact checkpoint.
    Preview(TaskSnapshotInspectArgs),
    /// Show the same bounded change set used for switch confirmation.
    Diff(TaskSnapshotInspectArgs),
    /// Create a recovery point and switch to one confirmed checkpoint.
    Switch(TaskSnapshotSwitchArgs),
}

#[derive(Debug, Clone, Args)]
struct TaskSnapshotInspectArgs {
    /// Canonical COSH Task ID.
    #[arg(value_name = "TASK_ID")]
    task_id: String,
    /// Complete COSH checkpoint ID; prefixes are not accepted.
    #[arg(value_name = "SNAPSHOT_ID")]
    snapshot_id: String,
}

#[derive(Debug, Clone, Args)]
struct TaskSnapshotSwitchArgs {
    /// Canonical COSH Task ID.
    #[arg(value_name = "TASK_ID")]
    task_id: String,
    /// Complete COSH checkpoint ID; prefixes are not accepted.
    #[arg(value_name = "SNAPSHOT_ID")]
    snapshot_id: String,
    /// Digest returned by the immediately preceding preview.
    #[arg(long, value_name = "SHA256")]
    preview_digest: String,
    /// Exact terminal Task revision displayed during preview.
    #[arg(long)]
    expected_revision: u64,
    /// Caller-stable replay key; never replace it after uncertain I/O.
    #[arg(long, value_name = "KEY")]
    idempotency_key: String,
}

#[derive(Debug, Clone, Args)]
struct TaskListArgs {
    /// Maximum newest-first Task projections returned.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=64))]
    limit: u16,
}

#[derive(Debug, Clone, Args)]
struct TaskSubmitArgs {
    /// Read Task intent from this regular file; default is stdin.
    #[arg(long, value_name = "PATH")]
    intent_file: Option<PathBuf>,
    /// Caller-stable replay key; generate once and reuse after uncertain I/O.
    #[arg(long, value_name = "KEY")]
    idempotency_key: String,
    /// Provider-neutral Runtime family selected for the durable Task.
    #[arg(long, value_enum, default_value_t = TaskRuntimeArg::Core)]
    runtime: TaskRuntimeArg,
    /// Pre-Runtime workspace baseline behavior.
    #[arg(long, value_enum, default_value_t = CheckpointArg::Auto)]
    checkpoint: CheckpointArg,
    /// Durable permission policy; supported operations remain audited.
    #[arg(long, value_enum, default_value_t = ApprovalPolicyArg::AllowAll)]
    approval_policy: ApprovalPolicyArg,
    /// Fail if the daemon's canonical workspace changed after confirmation.
    #[arg(long, value_name = "SHA256", value_parser = parse_workspace_digest)]
    expected_workspace_digest: Option<String>,
}

fn parse_workspace_digest(value: &str) -> Result<String, String> {
    cosh_gateway_contracts::common::Digest::parse(value)
        .map(|digest| digest.as_str().to_owned())
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum TaskRuntimeArg {
    #[default]
    Core,
    Codex,
}

impl From<TaskRuntimeArg> for TaskRuntime {
    fn from(value: TaskRuntimeArg) -> Self {
        match value {
            TaskRuntimeArg::Core => Self::Core,
            TaskRuntimeArg::Codex => Self::Codex,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum CheckpointArg {
    #[default]
    Auto,
    On,
    Off,
}

impl From<CheckpointArg> for CheckpointPolicy {
    fn from(value: CheckpointArg) -> Self {
        match value {
            CheckpointArg::Auto => Self::Auto,
            CheckpointArg::On => Self::On,
            CheckpointArg::Off => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum ApprovalPolicyArg {
    #[default]
    AllowAll,
}

impl From<ApprovalPolicyArg> for ApprovalPolicy {
    fn from(value: ApprovalPolicyArg) -> Self {
        match value {
            ApprovalPolicyArg::AllowAll => Self::AllowAll,
        }
    }
}

#[derive(Debug, Clone, Args)]
struct TaskIdArgs {
    /// Canonical COSH Task ID.
    #[arg(value_name = "TASK_ID")]
    task_id: String,
}

#[derive(Debug, Clone, Args)]
struct TaskCancelArgs {
    /// Canonical COSH Task ID.
    #[arg(value_name = "TASK_ID")]
    task_id: String,
    /// Active Run being cancelled.
    #[arg(long, value_name = "RUN_ID")]
    run_id: String,
    /// Caller-stable replay key; generate once and reuse after uncertain I/O.
    #[arg(long, value_name = "KEY")]
    idempotency_key: String,
    /// Reject cancellation if the Task has advanced beyond this revision.
    #[arg(long)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Args)]
struct TaskRetryArgs {
    /// Canonical COSH Task ID.
    #[arg(value_name = "TASK_ID")]
    task_id: String,
    /// Exact suspended Run being replaced.
    #[arg(long, value_name = "RUN_ID")]
    previous_run_id: String,
    /// Caller-stable replay key; generate once and reuse after uncertain I/O.
    #[arg(long, value_name = "KEY")]
    idempotency_key: String,
    /// Reject retry if the Task has advanced beyond this revision.
    #[arg(long)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Args)]
struct TaskResolveApprovalArgs {
    /// Canonical approval identity from the Task event stream.
    #[arg(value_name = "APPROVAL_ID")]
    approval_id: String,
    /// Approve once or deny the pending operation.
    #[arg(long, value_enum)]
    decision: ApprovalChoice,
    /// Caller-stable replay key; generate once and reuse after uncertain I/O.
    #[arg(long, value_name = "KEY")]
    idempotency_key: String,
}

#[derive(Debug, Clone, Args)]
struct TaskAppendArgs {
    /// Canonical COSH Task ID.
    #[arg(value_name = "TASK_ID")]
    task_id: String,
    /// Exact input request identity from the Task event stream.
    #[arg(long, value_name = "INPUT_REQUEST_ID")]
    input_request_id: String,
    /// Read free-text input from this regular file; default is stdin.
    #[arg(long, value_name = "PATH", conflicts_with = "selections")]
    input_file: Option<PathBuf>,
    /// Select one zero-based option index; repeat for multi-select questions.
    #[arg(long = "select", value_name = "INDEX")]
    selections: Vec<u16>,
    /// Caller-stable replay key; generate once and reuse after uncertain I/O.
    #[arg(long, value_name = "KEY")]
    idempotency_key: String,
    /// Reject append if the Task has advanced beyond this revision.
    #[arg(long)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Args)]
struct TaskEventsArgs {
    /// Canonical COSH Task ID.
    #[arg(value_name = "TASK_ID")]
    task_id: String,
    /// Last durable Task revision already observed.
    #[arg(long, default_value_t = 0)]
    after: u64,
    /// Maximum events returned in one bounded page.
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u16).range(1..=64))]
    limit: u16,
}

#[derive(Debug, Clone, Args)]
struct ProfileArgs {
    /// Fixed installed adapter profile.
    #[arg(long, value_enum, default_value_t = Profile::Codex)]
    profile: Profile,
    /// Absolute trusted adapter path; basename must match the profile.
    #[arg(long)]
    adapter: Option<PathBuf>,
    /// Existing workspace directory bound to the ACP session.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// Presentation format for stable COSH events and errors.
    #[arg(long, value_enum, default_value_t = Output::Human)]
    output: Output,
}

#[derive(Debug, Clone, Args)]
struct RunArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    /// Read the prompt from this regular file; default is stdin.
    #[arg(long, value_name = "PATH")]
    prompt_file: Option<PathBuf>,
    /// Prompt on the local controlling terminal or deny every tool request.
    #[arg(long, value_enum, default_value_t = PermissionMode::Prompt)]
    permission: PermissionMode,
    /// Absolute private JSONL evidence path; defaults below the user state directory.
    #[arg(long, value_name = "PATH")]
    permission_evidence: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum Profile {
    #[default]
    Codex,
    ClaudeCode,
}

impl From<Profile> for AcpRuntimeProfileId {
    fn from(profile: Profile) -> Self {
        match profile {
            Profile::Codex => Self::Codex,
            Profile::ClaudeCode => Self::ClaudeCode,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum Output {
    #[default]
    Human,
    Jsonl,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum PermissionMode {
    /// Ask on `/dev/tty`; cancel when no controlling terminal is available.
    #[default]
    Prompt,
    /// Cancel every permission callback without presenting it.
    Deny,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ApprovalChoice {
    Approve,
    Deny,
}

impl From<ApprovalChoice> for ApprovalDecision {
    fn from(value: ApprovalChoice) -> Self {
        match value {
            ApprovalChoice::Approve => Self::Approve,
            ApprovalChoice::Deny => Self::Deny,
        }
    }
}

#[derive(Debug, Error)]
enum CliError {
    #[error("failed to resolve installed ACP profile: {0}")]
    Profile(String),
    #[error("failed to read prompt: {0}")]
    Input(#[source] io::Error),
    #[error("invalid control request: {0}")]
    InvalidInput(String),
    #[error("failed to read task intent: {0}")]
    IntentInput(#[source] io::Error),
    #[error("prompt path is not a regular file: {0}")]
    PromptNotRegular(PathBuf),
    #[error("prompt is empty")]
    EmptyPrompt,
    #[error("prompt exceeds the {MAX_PROMPT_BYTES}-byte limit")]
    PromptTooLarge,
    #[error("task intent path is not a regular file: {0}")]
    IntentNotRegular(PathBuf),
    #[error("task intent is empty")]
    EmptyIntent,
    #[error("task intent exceeds the {MAX_PROMPT_BYTES}-byte limit")]
    IntentTooLarge,
    #[error("failed to register interrupt handling: {0}")]
    Signal(#[source] io::Error),
    #[error("local permission handling failed: {0}")]
    Permission(String),
    #[error("ACP runtime failed: {0}")]
    Runtime(String),
    #[error("Gateway daemon request failed: {0}")]
    Daemon(String),
    #[error("Gateway Runtime containment failed: {0}")]
    Containment(String),
    #[error("Gateway store inspection failed: {0}")]
    StoreInspection(String),
    #[error("local Web beta failed: {0}")]
    #[cfg(target_os = "linux")]
    Web(String),
    #[error("ACP Agent rejected or did not complete the prompt")]
    Agent,
    #[error("ACP operation was cancelled")]
    Cancelled,
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Input(_)
            | Self::InvalidInput(_)
            | Self::IntentInput(_)
            | Self::PromptNotRegular(_)
            | Self::EmptyPrompt
            | Self::PromptTooLarge
            | Self::IntentNotRegular(_)
            | Self::EmptyIntent
            | Self::IntentTooLarge => EXIT_INPUT,
            Self::Profile(_) => EXIT_PROFILE,
            Self::Runtime(_)
            | Self::Daemon(_)
            | Self::Containment(_)
            | Self::StoreInspection(_)
            | Self::Signal(_)
            | Self::Permission(_) => EXIT_RUNTIME,
            #[cfg(target_os = "linux")]
            Self::Web(_) => EXIT_RUNTIME,
            Self::Agent => EXIT_AGENT,
            Self::Cancelled => EXIT_CANCELLED,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Input(_) => "prompt_read_failed",
            Self::InvalidInput(_) => "invalid_request",
            Self::IntentInput(_) => "intent_read_failed",
            Self::PromptNotRegular(_) => "prompt_not_regular",
            Self::EmptyPrompt => "prompt_empty",
            Self::PromptTooLarge => "prompt_too_large",
            Self::IntentNotRegular(_) => "intent_not_regular",
            Self::EmptyIntent => "intent_empty",
            Self::IntentTooLarge => "intent_too_large",
            Self::Profile(_) => "profile_invalid",
            Self::Signal(_) => "signal_handler_failed",
            Self::Permission(_) => "permission_failed",
            Self::Runtime(_) => "runtime_failed",
            Self::Daemon(_) => "daemon_failed",
            Self::Containment(_) => "runtime_containment_unverified",
            Self::StoreInspection(_) => "store_inspection_failed",
            #[cfg(target_os = "linux")]
            Self::Web(_) => "web_failed",
            Self::Agent => "agent_incomplete",
            Self::Cancelled => "cancelled",
        }
    }
}

struct Reporter {
    output: Output,
}

impl Reporter {
    fn event(&self, event: &str, fields: Value) -> Result<(), CliError> {
        match self.output {
            Output::Jsonl => {
                let mut value = json!({"event": event});
                if let (Some(target), Some(source)) = (value.as_object_mut(), fields.as_object()) {
                    target.extend(source.clone());
                }
                println!("{value}");
            }
            Output::Human => self.human_event(event, &fields),
        }
        io::stdout()
            .flush()
            .map_err(|error| CliError::Runtime(error.to_string()))
    }

    fn human_event(&self, event: &str, fields: &Value) {
        match event {
            "initialized" => eprintln!("ACP v1 initialized"),
            "session_opened" => eprintln!("ACP session opened"),
            "session_update" => {
                if let Some(text) = fields.get("text").and_then(Value::as_str) {
                    print!("{}", terminal_safe(text));
                }
            }
            "permission_decided" => match fields.get("decision").and_then(Value::as_str) {
                Some("allow_once") => eprintln!("ACP permission allowed once"),
                Some("reject_once") => eprintln!("ACP permission rejected once"),
                _ => eprintln!("ACP permission request cancelled"),
            },
            "permission_callbacks_abandoned" => {
                eprintln!("ACP prompt cancelled with pending permission callbacks")
            }
            "prompt_finished" => eprintln!("\nACP prompt finished"),
            "doctor_ok" => println!("ACP adapter is ready"),
            "terminal" => {}
            "daemon_ready" => eprintln!("COSH Gateway daemon is ready"),
            "web_ready" => {
                if let Some(url) = fields.get("url").and_then(Value::as_str) {
                    println!("COSH local Task Web beta: {url}");
                }
            }
            "task_submitted" => print_task_id(fields),
            "task" => println!("{}", human_json(fields)),
            "tasks" => println!("{}", human_json(fields)),
            "task_events" => println!("{}", human_json(fields)),
            "task_capabilities" => println!("{}", human_json(fields)),
            "task_snapshots" | "task_snapshot_preview" | "task_snapshot_switched" => {
                println!("{}", human_json(fields));
            }
            "task_cancelled" => print_task_id(fields),
            "store_inspection" => println!("{}", human_json(fields)),
            _ => {}
        }
    }

    fn error(&self, error: &CliError) {
        match self.output {
            Output::Human => eprintln!("Error [{}]: {error}", error.code()),
            Output::Jsonl => println!(
                "{}",
                json!({"event":"error", "code":error.code(), "message":error.to_string()})
            ),
        }
    }
}

fn print_task_id(fields: &Value) {
    if let Some(task_id) = fields.get("task_id").and_then(Value::as_str) {
        println!("{task_id}");
    }
}

fn human_json(fields: &Value) -> String {
    serde_json::to_string_pretty(fields).unwrap_or_else(|_| "{}".to_owned())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let output = match &cli.command {
        Command::Doctor(args) => args.output,
        Command::Run(args) => args.profile.output,
        Command::Serve(args) => args.output,
        Command::Task(args) => args.output,
        Command::Admin(args) => args.output,
        #[cfg(target_os = "linux")]
        Command::Web(args) => args.output,
    };
    let reporter = Reporter { output };
    let result = match cli.command {
        Command::Doctor(args) => doctor(args, &reporter),
        Command::Run(args) => run(args, &reporter),
        Command::Serve(args) => serve(args, &reporter),
        Command::Task(args) => task(args, &reporter),
        Command::Admin(args) => admin(args, &reporter),
        #[cfg(target_os = "linux")]
        Command::Web(args) => web(args, &reporter),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            reporter.error(&error);
            ExitCode::from(error.exit_code())
        }
    }
}

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Codex => "codex",
        Profile::ClaudeCode => "claude-code",
    }
}

fn stop_reason_name(reason: AcpV1StopReason) -> &'static str {
    match reason {
        AcpV1StopReason::EndTurn => "end_turn",
        AcpV1StopReason::MaxTokens => "max_tokens",
        AcpV1StopReason::MaxTurnRequests => "max_turn_requests",
        AcpV1StopReason::Refusal => "refusal",
        AcpV1StopReason::Cancelled => "cancelled",
        AcpV1StopReason::Unsupported => "unsupported",
    }
}

#[cfg(test)]
#[path = "cosh_gateway/tests.rs"]
mod tests;
