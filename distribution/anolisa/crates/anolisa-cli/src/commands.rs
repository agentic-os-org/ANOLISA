//! Command-line surface.
//!
//! Two-tier structure (see design doc):
//! - **Tier 1** — component lifecycle verbs for everyday use (`tier1/`).
//! - **Tier 2** — independent management surfaces (register / adapter / self
//!   / runtime / osbase). Each surface uses its own appropriate vocabulary.

pub mod common;
pub(crate) mod state_view;
pub mod tier1;

// Tier 2 surfaces
pub mod adapter;
pub mod osbase;
pub mod register;
pub mod system;
pub mod telemetry;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, Subcommand};

use crate::context::{CliContext, InstallMode};
use crate::response::CliError;

const HELP_TEMPLATE: &str = "\
{before-help}{name} {version}\n\
{about-with-newline}\n\
{usage-heading} {usage}\
{after-help}\
\nOptions:\n{options}";

#[derive(Parser)]
#[command(
    name = "anolisa",
    about = "ANOLISA — Agentic OS helper",
    version,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Install scope: user (~/.local) or system (/usr/local).
    /// When omitted, defaults to system if running as root, user otherwise.
    #[arg(long, global = true, value_enum)]
    pub install_mode: Option<InstallMode>,

    /// Custom system install prefix, including user-mode system visibility
    #[arg(long, global = true, value_name = "PATH")]
    pub prefix: Option<PathBuf>,

    /// Output in JSON format
    #[arg(long, global = true)]
    pub json: bool,

    /// Print plan without executing
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Increase verbosity
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Suppress non-error output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    #[command(flatten)]
    Component(ComponentCommands),

    #[command(flatten)]
    Management(ManagementCommands),
}

/// Primary commands — component lifecycle and operations.
#[derive(Subcommand)]
pub enum ComponentCommands {
    /// List available components from the component index
    #[command(visible_alias = "ls")]
    List(tier1::list::ListArgs),
    /// Install a component from a configured raw or RPM backend
    Install(tier1::install::InstallArgs),
    /// Uninstall a component
    Uninstall(tier1::uninstall::UninstallArgs),
    /// Show component health
    Status(tier1::status::StatusArgs),
    /// Diagnose component issues
    Doctor(tier1::doctor::DoctorArgs),
    /// Query logs, optionally filtered by component and level
    Logs(tier1::logs::LogsArgs),
    /// Restart a component's service
    Restart(tier1::restart::RestartArgs),
    /// Update a component (`update <component>`), the CLI binary (`self`), or everything (`all`)
    Update(tier1::update::UpdateArgs),
    /// Upgrade the RPM/system image to the target toolchain profile (system-only)
    Upgrade(tier1::upgrade::UpgradeArgs),
    /// Reconcile a component's ANOLISA state with rpmdb after manual RPM changes
    Repair(tier1::repair::RepairArgs),
    /// Drop a component's ANOLISA state record without any package operation
    Forget(tier1::forget::ForgetArgs),
    /// Record an already-installed system RPM as adopted (system scope)
    Adopt(tier1::adopt::AdoptArgs),
    /// Manage component-to-framework adapters
    Adapter(adapter::AdapterArgs),
}

/// Management commands.
#[derive(Subcommand)]
pub enum ManagementCommands {
    /// Join the Agentic OS Co-Build Program (requires root/sudo)
    Register(register::RegisterArgs),
    /// Leave the Agentic OS Co-Build Program (requires root/sudo)
    Unregister(register::UnregisterArgs),
    /// Show environment detection results
    Env(tier1::env::EnvArgs),
    /// Generate a bug report
    Bug(tier1::bug::BugArgs),
    /// Manage OS base layer (kernel / sandbox / security)
    Osbase(osbase::OsbaseArgs),
    /// System helper daemon management
    System(system::SystemArgs),
    /// Manage telemetry collection and reporting
    Telemetry(telemetry::TelemetryArgs),
}

/// Build the top-level [`clap::Command`] with grouped help rendering.
///
/// Generates the "Component Commands / Management Commands / Other"
/// sections dynamically from the registered subcommands so that adding
/// a new variant to [`ComponentCommands`] or [`ManagementCommands`]
/// automatically updates `--help` without maintaining a separate const.
pub fn build_cli() -> clap::Command {
    let cmd = Cli::command();
    let help_text = generate_grouped_help();
    cmd.help_template(HELP_TEMPLATE).after_help(help_text)
}

fn generate_grouped_help() -> String {
    let cap = subcommand_rows::<ComponentCommands>();
    let mgmt = subcommand_rows::<ManagementCommands>();
    render_grouped_help(&cap, &mgmt)
}

fn subcommand_rows<T: Subcommand>() -> Vec<(String, String)> {
    let cmd = T::augment_subcommands(clap::Command::new("group"));
    let mut rows = Vec::new();
    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        let aliases: Vec<&str> = sub.get_visible_aliases().collect();
        let display = if aliases.is_empty() {
            name.to_string()
        } else {
            format!("{name}, {}", aliases.join(", "))
        };
        let about = sub.get_about().map(|s| s.to_string()).unwrap_or_default();

        rows.push((display, about));
    }
    rows
}

fn render_grouped_help(cap: &[(String, String)], mgmt: &[(String, String)]) -> String {
    let longest = cap
        .iter()
        .chain(mgmt)
        .map(|(d, _)| d.len())
        .max()
        .unwrap_or(0)
        .max(4); // "help" length

    let mut out = String::from("Commands:\n");
    for (display, about) in cap {
        let _ = writeln!(out, "  {display:<longest$}  {about}");
    }
    out.push_str("\nManagement Commands:\n");
    for (display, about) in mgmt {
        let _ = writeln!(out, "  {display:<longest$}  {about}");
    }
    out.push_str("\nOther:\n");
    let _ = writeln!(
        out,
        "  {:<longest$}  Print this message or the help of the given subcommand(s)",
        "help"
    );
    out
}

/// Dispatch parsed CLI arguments to their handlers.
///
/// Every handler receives the immutable [`CliContext`] so global flags
/// such as `--json`, `--dry-run`, `--install-mode` stay consistent across
/// surfaces. Handlers must not re-parse global flags from their own
/// `args` struct.
pub fn dispatch(cli: Cli, ctx: &CliContext) -> Result<(), CliError> {
    let policy = command_policy(&cli.command);
    validate_command_args(&cli, ctx, policy)?;
    match cli.command {
        Commands::Component(cmd) => match cmd {
            ComponentCommands::List(args) => tier1::list::handle(args, ctx),
            ComponentCommands::Install(args) => tier1::install::handle(args, ctx),
            ComponentCommands::Uninstall(args) => tier1::uninstall::handle(args, ctx),
            ComponentCommands::Status(args) => tier1::status::handle(args, ctx),
            ComponentCommands::Doctor(args) => tier1::doctor::handle(args, ctx),
            ComponentCommands::Logs(args) => tier1::logs::handle(args, ctx),
            ComponentCommands::Restart(args) => tier1::restart::handle(args, ctx),
            ComponentCommands::Update(args) => tier1::update::handle(args, ctx),
            ComponentCommands::Upgrade(args) => tier1::upgrade::handle(args, ctx),
            ComponentCommands::Repair(args) => tier1::repair::handle(args, ctx),
            ComponentCommands::Forget(args) => tier1::forget::handle(args, ctx),
            ComponentCommands::Adopt(args) => tier1::adopt::handle(args, ctx),
            ComponentCommands::Adapter(args) => adapter::handle(args, ctx),
        },
        Commands::Management(cmd) => match cmd {
            ManagementCommands::Register(args) => register::handle_register_group(args, ctx),
            ManagementCommands::Unregister(args) => register::handle_unregister_cmd(args, ctx),
            ManagementCommands::Env(args) => tier1::env::handle(args, ctx),
            ManagementCommands::Bug(args) => tier1::bug::handle(args, ctx),
            ManagementCommands::Osbase(args) => osbase::handle(args, ctx),
            ManagementCommands::System(args) => system::handle(args, ctx),
            ManagementCommands::Telemetry(args) => telemetry::handle(args, ctx),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DryRunPolicy {
    Unsupported,
    PrivilegedPreview,
    UnprivilegedPreview,
}

impl DryRunPolicy {
    const fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported)
    }

    const fn waives_root(self) -> bool {
        matches!(self, Self::UnprivilegedPreview)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandScope {
    ReadOnly,
    ModeScopedMutation { dry_run: DryRunPolicy },
    SystemOnlyMutation { dry_run: DryRunPolicy },
    HelperGatedSystemOperation { dry_run: DryRunPolicy },
}

impl CommandScope {
    const fn dry_run_policy(self) -> Option<DryRunPolicy> {
        match self {
            Self::ReadOnly => None,
            Self::ModeScopedMutation { dry_run }
            | Self::SystemOnlyMutation { dry_run }
            | Self::HelperGatedSystemOperation { dry_run } => Some(dry_run),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandPolicy {
    label: &'static str,
    scope: CommandScope,
}

impl CommandPolicy {
    const fn new(label: &'static str, scope: CommandScope) -> Self {
        Self { label, scope }
    }
}

fn validate_command_args(
    cli: &Cli,
    ctx: &CliContext,
    policy: CommandPolicy,
) -> Result<(), CliError> {
    validate_command_args_with_euid(
        ctx,
        policy,
        anolisa_platform::privilege::effective_uid(),
        cli.install_mode.is_some(),
        ctx.dry_run || command_local_dry_run(&cli.command),
    )
}

#[cfg(test)]
fn validate_global_args_with_euid(
    ctx: &CliContext,
    policy: CommandPolicy,
    effective_uid: u32,
    install_mode_explicit: bool,
) -> Result<(), CliError> {
    validate_command_args_with_euid(
        ctx,
        policy,
        effective_uid,
        install_mode_explicit,
        ctx.dry_run,
    )
}

fn validate_command_args_with_euid(
    ctx: &CliContext,
    policy: CommandPolicy,
    effective_uid: u32,
    install_mode_explicit: bool,
    dry_run_requested: bool,
) -> Result<(), CliError> {
    if let Some(prefix) = &ctx.prefix
        && !is_safe_absolute_path(prefix)
    {
        return Err(CliError::InvalidArgument {
            command: "global".to_string(),
            reason: format!(
                "--prefix must be an absolute path without '.' or '..' segments, got {}",
                prefix.display()
            ),
        });
    }

    match policy.scope {
        CommandScope::SystemOnlyMutation { .. } if ctx.install_mode != InstallMode::System => {
            return Err(system_scope_error(policy.label, install_mode_explicit));
        }
        CommandScope::HelperGatedSystemOperation { .. }
            if install_mode_explicit && ctx.install_mode != InstallMode::System =>
        {
            return Err(system_scope_error(policy.label, true));
        }
        _ => {}
    }

    if dry_run_requested
        && policy
            .scope
            .dry_run_policy()
            .is_some_and(|dry_run| !dry_run.is_supported())
    {
        return Err(unsupported_dry_run_error(policy.label));
    }

    match policy.scope {
        CommandScope::ReadOnly => {}
        CommandScope::ModeScopedMutation { dry_run } => {
            let dry_run_preview = dry_run_requested && dry_run.waives_root();
            if ctx.install_mode == InstallMode::System && effective_uid != 0 && !dry_run_preview {
                return Err(system_permission_error(ctx, policy.label, true));
            }
        }
        CommandScope::SystemOnlyMutation { dry_run } => {
            let dry_run_preview = dry_run_requested && dry_run.waives_root();
            if effective_uid != 0 && !dry_run_preview {
                return Err(system_permission_error(ctx, policy.label, false));
            }
        }
        CommandScope::HelperGatedSystemOperation { .. } => {}
    }

    if ctx.install_mode == InstallMode::User && effective_uid == 0 {
        return Err(root_user_mode_error(policy.label));
    }

    Ok(())
}

fn command_local_dry_run(command: &Commands) -> bool {
    let Commands::Management(ManagementCommands::Osbase(args)) = command else {
        return false;
    };

    match &args.command {
        osbase::OsbaseCommands::Kernel(kernel) => match &kernel.command {
            osbase::KernelCommands::Install { dry_run } => *dry_run,
            osbase::KernelCommands::Remove | osbase::KernelCommands::Status => false,
        },
        osbase::OsbaseCommands::Sandbox(sandbox) => match &sandbox.command {
            osbase::SandboxCommands::Install { dry_run, .. }
            | osbase::SandboxCommands::Uninstall { dry_run, .. }
            | osbase::SandboxCommands::Remove { dry_run, .. } => *dry_run,
            osbase::SandboxCommands::List { .. } | osbase::SandboxCommands::Status { .. } => false,
        },
        osbase::OsbaseCommands::Security(security) => match &security.command {
            osbase::SecurityCommands::Install { dry_run, .. } => *dry_run,
            osbase::SecurityCommands::Remove { .. } | osbase::SecurityCommands::Status { .. } => {
                false
            }
        },
    }
}

pub(crate) fn unsupported_dry_run_error(command: &str) -> CliError {
    CliError::InvalidArgument {
        command: command.to_string(),
        reason: format!("command '{command}' does not support --dry-run; no action was taken"),
    }
}

fn root_user_mode_error(command: &str) -> CliError {
    CliError::InvalidArgument {
        command: command.to_string(),
        reason: format!(
            "--install-mode user is not supported while running as root; run `anolisa --install-mode user {command} ...` without sudo, or omit `--install-mode user` when using sudo for system mode"
        ),
    }
}

fn system_scope_error(command: &str, install_mode_explicit: bool) -> CliError {
    let reason =
        format!("command '{command}' operates on system scope, but current install mode is user");
    if install_mode_explicit {
        CliError::InvalidArgument {
            command: command.to_string(),
            reason,
        }
    } else {
        CliError::PermissionDenied {
            command: command.to_string(),
            reason,
            hint: Some(format!(
                "run `sudo anolisa {command} ...`; sudo defaults this command to system mode"
            )),
        }
    }
}

const fn mode_scoped(label: &'static str, dry_run: DryRunPolicy) -> CommandPolicy {
    CommandPolicy::new(label, CommandScope::ModeScopedMutation { dry_run })
}

const fn system_only(label: &'static str, dry_run: DryRunPolicy) -> CommandPolicy {
    CommandPolicy::new(label, CommandScope::SystemOnlyMutation { dry_run })
}

fn system_permission_error(ctx: &CliContext, command: &str, user_mode_supported: bool) -> CliError {
    let target = ctx
        .prefix
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "/usr/local".to_string());
    let hint = if user_mode_supported {
        format!(
            "run `sudo anolisa {command} ...` for system mode, or `anolisa --install-mode user {command} ...` for user mode"
        )
    } else {
        format!("run `sudo anolisa {command} ...`; sudo defaults this command to system mode")
    };
    CliError::PermissionDenied {
        command: command.to_string(),
        reason: format!("system mode needs write access to {target} and requires root"),
        hint: Some(hint),
    }
}

fn command_policy(command: &Commands) -> CommandPolicy {
    match command {
        Commands::Component(cmd) => match cmd {
            ComponentCommands::List(_) => CommandPolicy::new("list", CommandScope::ReadOnly),
            ComponentCommands::Install(_) => {
                mode_scoped("install", DryRunPolicy::UnprivilegedPreview)
            }
            ComponentCommands::Uninstall(_) => {
                mode_scoped("uninstall", DryRunPolicy::UnprivilegedPreview)
            }
            ComponentCommands::Status(_) => CommandPolicy::new("status", CommandScope::ReadOnly),
            ComponentCommands::Doctor(_) => CommandPolicy::new("doctor", CommandScope::ReadOnly),
            ComponentCommands::Logs(_) => CommandPolicy::new("logs", CommandScope::ReadOnly),
            // Preview lists recorded/discovered units and does not
            // daemon-reload, restart, or take the exclusive install lock,
            // so `--dry-run` can waive root like install/uninstall/repair/forget.
            ComponentCommands::Restart(_) => {
                mode_scoped("restart", DryRunPolicy::UnprivilegedPreview)
            }
            // `update --check` is read-only upgrade detection: it only runs
            // read-only rpm/dnf queries (no package transaction, no state
            // writes), so it must not be gated behind the mutating-update root
            // requirement — the MOTD hook runs it unprivileged.
            ComponentCommands::Update(args) if args.check => {
                CommandPolicy::new("update", CommandScope::ReadOnly)
            }
            ComponentCommands::Update(_) => {
                mode_scoped("update", DryRunPolicy::UnprivilegedPreview)
            }
            // `upgrade` is a system-only RPM image mutation: real execution
            // needs root. `--dry-run` waives only the root requirement, not the
            // system-mode one, so an explicit system-mode dry-run can preview
            // the plan without root (matching `repair`).
            ComponentCommands::Upgrade(_) => {
                system_only("upgrade", DryRunPolicy::UnprivilegedPreview)
            }
            ComponentCommands::Repair(_) => {
                mode_scoped("repair", DryRunPolicy::UnprivilegedPreview)
            }
            ComponentCommands::Forget(_) => {
                mode_scoped("forget", DryRunPolicy::UnprivilegedPreview)
            }
            ComponentCommands::Adopt(_) => system_only("adopt", DryRunPolicy::UnprivilegedPreview),
            ComponentCommands::Adapter(args) => {
                CommandPolicy::new("adapter", adapter_command_scope(args))
            }
        },
        Commands::Management(cmd) => match cmd {
            ManagementCommands::Register(args) => {
                CommandPolicy::new("register", register_command_scope(args))
            }
            ManagementCommands::Unregister(_) => {
                system_only("unregister", DryRunPolicy::Unsupported)
            }
            ManagementCommands::Env(_) => CommandPolicy::new("env", CommandScope::ReadOnly),
            ManagementCommands::Bug(_) => CommandPolicy::new("bug", CommandScope::ReadOnly),
            ManagementCommands::Osbase(args) => {
                let label = match &args.command {
                    osbase::OsbaseCommands::Kernel(osbase::KernelArgs {
                        command: osbase::KernelCommands::Install { .. },
                    }) => "osbase kernel install",
                    osbase::OsbaseCommands::Sandbox(osbase::SandboxArgs {
                        command: osbase::SandboxCommands::Remove { .. },
                    }) => "osbase sandbox remove",
                    osbase::OsbaseCommands::Security(osbase::SecurityArgs {
                        command: osbase::SecurityCommands::Install { .. },
                    }) => "osbase security install",
                    _ => "osbase",
                };
                CommandPolicy::new(label, osbase_command_scope(args))
            }
            ManagementCommands::System(args) => {
                CommandPolicy::new("system", system_command_scope(args))
            }
            ManagementCommands::Telemetry(args) => {
                CommandPolicy::new("telemetry", telemetry_command_scope(args))
            }
        },
    }
}

fn telemetry_command_scope(args: &telemetry::TelemetryArgs) -> CommandScope {
    match &args.command {
        telemetry::TelemetryCommands::Status { .. } => CommandScope::ReadOnly,
        // enable / disable / link / unlink / upload / init mutate system state
        // and need root; an explicit system-mode dry-run may preview without root.
        telemetry::TelemetryCommands::Enable
        | telemetry::TelemetryCommands::Disable
        | telemetry::TelemetryCommands::Link
        | telemetry::TelemetryCommands::Unlink
        | telemetry::TelemetryCommands::Upload { .. }
        | telemetry::TelemetryCommands::Init => CommandScope::ModeScopedMutation {
            dry_run: DryRunPolicy::UnprivilegedPreview,
        },
    }
}

fn adapter_command_scope(args: &adapter::AdapterArgs) -> CommandScope {
    match &args.command {
        adapter::AdapterCommands::Scan | adapter::AdapterCommands::Status { .. } => {
            CommandScope::ReadOnly
        }
        // Keep adapter previews privileged in system mode for compatibility
        // with the existing mutation authorization policy.
        adapter::AdapterCommands::Enable { .. } | adapter::AdapterCommands::Disable { .. } => {
            CommandScope::ModeScopedMutation {
                dry_run: DryRunPolicy::PrivilegedPreview,
            }
        }
    }
}

fn register_command_scope(args: &register::RegisterArgs) -> CommandScope {
    match &args.command {
        Some(register::RegisterCommands::Status { .. }) => CommandScope::ReadOnly,
        None => system_only_scope(DryRunPolicy::Unsupported),
    }
}

fn system_command_scope(args: &system::SystemArgs) -> CommandScope {
    match &args.command {
        system::SystemCommands::Status { .. } => CommandScope::ReadOnly,
        system::SystemCommands::Serve { .. }
        | system::SystemCommands::Setup { .. }
        | system::SystemCommands::Teardown => system_only_scope(DryRunPolicy::Unsupported),
    }
}

fn osbase_command_scope(args: &osbase::OsbaseArgs) -> CommandScope {
    match &args.command {
        osbase::OsbaseCommands::Kernel(kernel) => match &kernel.command {
            osbase::KernelCommands::Status => CommandScope::ReadOnly,
            osbase::KernelCommands::Install { .. } | osbase::KernelCommands::Remove => {
                helper_gated_system_operation_scope(DryRunPolicy::Unsupported)
            }
        },
        osbase::OsbaseCommands::Sandbox(sandbox) => match &sandbox.command {
            osbase::SandboxCommands::List { .. } | osbase::SandboxCommands::Status { .. } => {
                CommandScope::ReadOnly
            }
            osbase::SandboxCommands::Install { .. } | osbase::SandboxCommands::Uninstall { .. } => {
                helper_gated_system_operation_scope(DryRunPolicy::UnprivilegedPreview)
            }
            osbase::SandboxCommands::Remove { .. } => {
                helper_gated_system_operation_scope(DryRunPolicy::Unsupported)
            }
        },
        osbase::OsbaseCommands::Security(security) => match &security.command {
            osbase::SecurityCommands::Status { .. } => CommandScope::ReadOnly,
            osbase::SecurityCommands::Install { .. } | osbase::SecurityCommands::Remove { .. } => {
                helper_gated_system_operation_scope(DryRunPolicy::Unsupported)
            }
        },
    }
}

const fn system_only_scope(dry_run: DryRunPolicy) -> CommandScope {
    CommandScope::SystemOnlyMutation { dry_run }
}

const fn helper_gated_system_operation_scope(dry_run: DryRunPolicy) -> CommandScope {
    CommandScope::HelperGatedSystemOperation { dry_run }
}

fn is_safe_absolute_path(path: &Path) -> bool {
    path.is_absolute() && !path.as_os_str().is_empty() && !has_dot_segment(path)
}

fn has_dot_segment(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.split(std::path::MAIN_SEPARATOR)
        .any(|segment| segment == "." || segment == "..")
}

#[cfg(test)]
mod tests {
    use clap::FromArgMatches as _;

    use super::*;

    fn ctx_with_prefix(prefix: PathBuf) -> CliContext {
        crate::test_support::context_for_root(
            Path::new("/tmp/anolisa-commands-validation"),
            InstallMode::System,
            Some(prefix),
            crate::test_support::TestContextOptions {
                quiet: false,
                no_color: false,
                ..Default::default()
            },
        )
    }

    fn user_ctx() -> CliContext {
        crate::test_support::context_for_root(
            Path::new("/tmp/anolisa-commands-validation"),
            InstallMode::User,
            None,
            crate::test_support::TestContextOptions {
                quiet: false,
                no_color: false,
                ..Default::default()
            },
        )
    }

    #[test]
    fn command_local_dry_run_covers_every_local_osbase_flag() {
        let commands: &[&[&str]] = &[
            &["anolisa", "osbase", "kernel", "install"],
            &["anolisa", "osbase", "sandbox", "install", "gvisor"],
            &["anolisa", "osbase", "sandbox", "uninstall", "gvisor"],
            &["anolisa", "osbase", "sandbox", "remove", "gvisor"],
            &["anolisa", "osbase", "security", "install", "loongshield"],
        ];

        for arguments in commands {
            let cli = Cli::try_parse_from(*arguments).expect("command without local dry-run");
            assert!(!command_local_dry_run(&cli.command));

            let mut with_dry_run = arguments.to_vec();
            with_dry_run.push("--dry-run");
            let cli = Cli::try_parse_from(with_dry_run).expect("command with local dry-run");
            assert!(command_local_dry_run(&cli.command));
        }

        let cli = Cli::try_parse_from(["anolisa", "osbase", "kernel", "status"])
            .expect("read-only command");
        assert!(!command_local_dry_run(&cli.command));
    }

    #[test]
    fn unsupported_osbase_install_dry_runs_match_for_local_global_root_and_non_root() {
        let cases: &[(&str, &[&str])] = &[
            (
                "osbase kernel install",
                &["anolisa", "osbase", "kernel", "install", "--dry-run"],
            ),
            (
                "osbase kernel install",
                &["anolisa", "--dry-run", "osbase", "kernel", "install"],
            ),
            (
                "osbase security install",
                &[
                    "anolisa",
                    "osbase",
                    "security",
                    "install",
                    "loongshield",
                    "--dry-run",
                ],
            ),
            (
                "osbase security install",
                &[
                    "anolisa",
                    "--dry-run",
                    "osbase",
                    "security",
                    "install",
                    "loongshield",
                ],
            ),
        ];

        for (label, arguments) in cases {
            let cli = Cli::try_parse_from(*arguments).expect("valid dry-run command");
            let mut ctx = ctx_with_prefix(PathBuf::from("/"));
            ctx.dry_run = cli.dry_run;
            let dry_run_requested = ctx.dry_run || command_local_dry_run(&cli.command);

            for effective_uid in [0, 1000] {
                let err = validate_command_args_with_euid(
                    &ctx,
                    command_policy(&cli.command),
                    effective_uid,
                    true,
                    dry_run_requested,
                )
                .expect_err("unsupported local and global dry-run must fail closed");

                assert_eq!(err.code(), "INVALID_ARGUMENT");
                assert_eq!(err.command(), *label);
                assert_eq!(
                    err.reason(),
                    format!("command '{label}' does not support --dry-run; no action was taken")
                );
            }
        }
    }

    #[test]
    fn global_prefix_must_be_absolute() {
        let err = validate_global_args_with_euid(
            &ctx_with_prefix(PathBuf::from("relative")),
            mode_scoped("global", DryRunPolicy::Unsupported),
            0,
            true,
        )
        .expect_err("relative prefix must be rejected");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }

    #[test]
    fn global_prefix_rejects_traversal_segments() {
        let err = validate_global_args_with_euid(
            &ctx_with_prefix(PathBuf::from("/opt/../etc")),
            mode_scoped("global", DryRunPolicy::Unsupported),
            0,
            true,
        )
        .expect_err("traversing prefix must be rejected");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }

    #[test]
    fn system_mode_without_root_is_rejected_before_writes() {
        let err = validate_global_args_with_euid(
            &ctx_with_prefix(PathBuf::from("/")),
            mode_scoped("install", DryRunPolicy::UnprivilegedPreview),
            1000,
            true,
        )
        .expect_err("non-root system mode must be rejected");

        assert_eq!(err.code(), "PERMISSION_DENIED");
        assert!(err.reason().contains("system mode"));
        assert!(err.hint().unwrap().contains("--install-mode user"));
    }

    #[test]
    fn system_mode_dry_run_without_root_is_allowed_for_previewing_commands() {
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        validate_global_args_with_euid(
            &ctx,
            mode_scoped("install", DryRunPolicy::UnprivilegedPreview),
            1000,
            true,
        )
        .expect("non-root system dry-run should reach preview-capable handlers");
    }

    #[test]
    fn restart_policy_allows_system_dry_run_without_root() {
        let command =
            Commands::Component(ComponentCommands::Restart(tier1::restart::RestartArgs {
                component: "cosh".to_string(),
            }));
        let policy = command_policy(&command);
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        validate_global_args_with_euid(&ctx, policy, 1000, true)
            .expect("non-root system restart dry-run should reach the preview handler");
        let err = validate_global_args_with_euid(
            &ctx_with_prefix(PathBuf::from("/")),
            policy,
            1000,
            true,
        )
        .expect_err("a real system restart still requires root");
        assert_eq!(err.code(), "PERMISSION_DENIED");
    }

    #[test]
    fn repair_policy_covers_user_and_system_owned_scopes() {
        let command = Commands::Component(ComponentCommands::Repair(tier1::repair::RepairArgs {
            component: "cosh".to_string(),
        }));
        let policy = command_policy(&command);

        validate_global_args_with_euid(&user_ctx(), policy, 1000, true)
            .expect("non-root user repair should reach its owned state");
        validate_global_args_with_euid(&ctx_with_prefix(PathBuf::from("/")), policy, 0, true)
            .expect("root system repair should reach its owned state");
        let err = validate_global_args_with_euid(
            &ctx_with_prefix(PathBuf::from("/")),
            policy,
            1000,
            true,
        )
        .expect_err("real system repair still requires root");
        assert_eq!(err.code(), "PERMISSION_DENIED");
    }

    #[test]
    fn system_only_dry_run_without_root_is_allowed_for_previewing_commands() {
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        validate_global_args_with_euid(
            &ctx,
            system_only("repair", DryRunPolicy::UnprivilegedPreview),
            1000,
            true,
        )
        .expect("non-root system dry-run should reach preview-capable system handlers");
    }

    #[test]
    fn telemetry_mutations_allow_non_root_system_dry_run_only() {
        let commands = [
            telemetry::TelemetryCommands::Enable,
            telemetry::TelemetryCommands::Disable,
            telemetry::TelemetryCommands::Link,
            telemetry::TelemetryCommands::Unlink,
            telemetry::TelemetryCommands::Upload { loop_flag: false },
            telemetry::TelemetryCommands::Upload { loop_flag: true },
            telemetry::TelemetryCommands::Init,
        ];

        for command in commands {
            let command =
                Commands::Management(ManagementCommands::Telemetry(telemetry::TelemetryArgs {
                    command,
                }));
            let policy = command_policy(&command);
            let mut preview = ctx_with_prefix(PathBuf::from("/"));
            preview.dry_run = true;

            validate_global_args_with_euid(&preview, policy, 1000, true)
                .expect("non-root telemetry dry-run should reach the preview dispatcher");
            let err = validate_global_args_with_euid(
                &ctx_with_prefix(PathBuf::from("/")),
                policy,
                1000,
                true,
            )
            .expect_err("telemetry apply still requires root in system mode");
            assert_eq!(err.code(), "PERMISSION_DENIED");
        }
    }

    #[test]
    fn adopt_policy_allows_non_root_system_preview() {
        let command = Commands::Component(ComponentCommands::Adopt(tier1::adopt::AdoptArgs {
            component: "cosh".to_string(),
            package: None,
        }));
        let policy = command_policy(&command);
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        assert_eq!(
            policy.scope.dry_run_policy(),
            Some(DryRunPolicy::UnprivilegedPreview)
        );
        validate_global_args_with_euid(&ctx, policy, 1000, true)
            .expect("adopt preview does not take the install lock or mutate state");
    }

    #[test]
    fn adapter_mutation_previews_retain_the_system_root_requirement() {
        let commands = [
            adapter::AdapterCommands::Enable {
                component: "tokenless".to_string(),
                framework: Some("openclaw".to_string()),
                allow_unsafe_plugin_install: false,
                profiles: Vec::new(),
            },
            adapter::AdapterCommands::Disable {
                component: "tokenless".to_string(),
                framework: Some("openclaw".to_string()),
            },
        ];
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        for adapter_command in commands {
            let command = Commands::Component(ComponentCommands::Adapter(adapter::AdapterArgs {
                command: adapter_command,
            }));
            let policy = command_policy(&command);

            assert_eq!(
                policy.scope.dry_run_policy(),
                Some(DryRunPolicy::PrivilegedPreview)
            );
            let err = validate_global_args_with_euid(&ctx, policy, 1000, true)
                .expect_err("adapter previews retain the system root policy");
            assert_eq!(err.code(), "PERMISSION_DENIED");
            validate_global_args_with_euid(&ctx, policy, 0, true)
                .expect("root should reach the adapter preview handler");
        }
    }

    #[test]
    fn unsupported_management_dry_runs_fail_closed_for_root() {
        let commands = [
            Commands::Management(ManagementCommands::Register(register::RegisterArgs {
                command: None,
                yes: true,
            })),
            Commands::Management(ManagementCommands::Unregister(register::UnregisterArgs {
                force: true,
            })),
            Commands::Management(ManagementCommands::System(system::SystemArgs {
                command: system::SystemCommands::Setup {
                    helper_path: None,
                    upgrade: false,
                },
            })),
        ];
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        for command in commands {
            let err = validate_global_args_with_euid(&ctx, command_policy(&command), 0, true)
                .expect_err("commands without preview implementations must fail closed");

            assert_eq!(err.code(), "INVALID_ARGUMENT");
            assert!(err.reason().contains("does not support --dry-run"));
        }
    }

    #[test]
    fn sandbox_install_and_uninstall_support_global_dry_run_but_remove_does_not() {
        let supported = [
            osbase::SandboxCommands::Install {
                target: "kata-containers".to_string(),
                dry_run: false,
                force: false,
                no_verify: false,
            },
            osbase::SandboxCommands::Uninstall {
                scenario: "kata-containers".to_string(),
                dry_run: false,
            },
        ];
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        for sandbox_command in supported {
            let command = Commands::Management(ManagementCommands::Osbase(osbase::OsbaseArgs {
                command: osbase::OsbaseCommands::Sandbox(osbase::SandboxArgs {
                    command: sandbox_command,
                }),
            }));
            let policy = command_policy(&command);

            assert_eq!(
                policy.scope.dry_run_policy(),
                Some(DryRunPolicy::UnprivilegedPreview)
            );
            validate_global_args_with_euid(&ctx, policy, 1000, true)
                .expect("sandbox install and uninstall forward dry-run to the helper request");
        }

        let remove = Commands::Management(ManagementCommands::Osbase(osbase::OsbaseArgs {
            command: osbase::OsbaseCommands::Sandbox(osbase::SandboxArgs {
                command: osbase::SandboxCommands::Remove {
                    target: "kata-containers".to_string(),
                    purge: false,
                    dry_run: false,
                },
            }),
        }));
        let err = validate_global_args_with_euid(&ctx, command_policy(&remove), 0, true)
            .expect_err("sandbox remove does not yet propagate dry-run");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("does not support --dry-run"));
    }

    #[test]
    fn unsupported_dry_run_is_rejected_before_execution() {
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        let err = validate_global_args_with_euid(
            &ctx,
            mode_scoped("restart", DryRunPolicy::Unsupported),
            0,
            true,
        )
        .expect_err("unsupported dry-run must fail closed even for root");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("does not support --dry-run"));
        assert!(err.reason().contains("no action was taken"));
    }

    #[test]
    fn privileged_preview_still_requires_root_in_system_mode() {
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        let policy = mode_scoped("adapter", DryRunPolicy::PrivilegedPreview);
        let err = validate_global_args_with_euid(&ctx, policy, 1000, true)
            .expect_err("privileged preview must retain the system root requirement");

        assert_eq!(err.code(), "PERMISSION_DENIED");
        validate_global_args_with_euid(&ctx, policy, 0, true)
            .expect("root should reach a privileged preview handler");
    }

    #[test]
    fn helper_gated_system_operation_reaches_handler_without_root_in_explicit_system_mode() {
        validate_global_args_with_euid(
            &ctx_with_prefix(PathBuf::from("/")),
            CommandPolicy::new(
                "osbase",
                helper_gated_system_operation_scope(DryRunPolicy::UnprivilegedPreview),
            ),
            1000,
            true,
        )
        .expect("helper-gated system operations should reach their handler preflight");
    }

    #[test]
    fn helper_gated_system_operation_reaches_handler_when_mode_is_omitted() {
        let command = Commands::Management(ManagementCommands::Osbase(osbase::OsbaseArgs {
            command: osbase::OsbaseCommands::Sandbox(osbase::SandboxArgs {
                command: osbase::SandboxCommands::Install {
                    target: "kata-containers".to_string(),
                    dry_run: false,
                    force: false,
                    no_verify: false,
                },
            }),
        }));

        validate_global_args_with_euid(&user_ctx(), command_policy(&command), 1000, false)
            .expect("omitted install mode should not block helper-gated system operations");
    }

    #[test]
    fn helper_gated_system_operation_rejects_explicit_user_mode() {
        let err = validate_global_args_with_euid(
            &user_ctx(),
            CommandPolicy::new(
                "osbase",
                helper_gated_system_operation_scope(DryRunPolicy::UnprivilegedPreview),
            ),
            1000,
            true,
        )
        .expect_err("explicit user mode is invalid for helper-gated system operations");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("system scope"));
        assert!(err.hint().is_none());
    }

    #[test]
    fn read_only_global_dry_run_without_root_is_not_preemptively_rejected() {
        let mut ctx = ctx_with_prefix(PathBuf::from("/"));
        ctx.dry_run = true;

        validate_global_args_with_euid(
            &ctx,
            CommandPolicy::new("status", CommandScope::ReadOnly),
            1000,
            true,
        )
        .expect("read-only commands should ignore global dry-run and reach their read path");
    }

    #[test]
    fn root_user_mode_is_rejected() {
        let err = validate_global_args_with_euid(
            &user_ctx(),
            mode_scoped("install", DryRunPolicy::UnprivilegedPreview),
            0,
            true,
        )
        .expect_err("root user-mode would create ambiguous ownership semantics");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("not supported while running as root"));
        assert!(err.reason().contains("without sudo"));
    }

    #[test]
    fn nested_read_only_commands_are_not_preemptively_rejected() {
        fn assert_read_only(command: Commands) {
            let policy = command_policy(&command);
            validate_global_args_with_euid(
                &ctx_with_prefix(PathBuf::from("/")),
                policy,
                1000,
                true,
            )
            .expect("nested read-only commands should reach their handlers");
        }

        assert_read_only(Commands::Component(ComponentCommands::Adapter(
            adapter::AdapterArgs {
                command: adapter::AdapterCommands::Status { component: None },
            },
        )));
        assert_read_only(Commands::Management(ManagementCommands::Register(
            register::RegisterArgs {
                command: Some(register::RegisterCommands::Status { json: false }),
                yes: false,
            },
        )));
        assert_read_only(Commands::Management(ManagementCommands::System(
            system::SystemArgs {
                command: system::SystemCommands::Status { json: false },
            },
        )));
        assert_read_only(Commands::Management(ManagementCommands::Telemetry(
            telemetry::TelemetryArgs {
                command: telemetry::TelemetryCommands::Status { json: false },
            },
        )));
        assert_read_only(Commands::Management(ManagementCommands::Osbase(
            osbase::OsbaseArgs {
                command: osbase::OsbaseCommands::Sandbox(osbase::SandboxArgs {
                    command: osbase::SandboxCommands::List { json: false },
                }),
            },
        )));
    }

    #[test]
    fn system_only_user_mode_points_at_sudo_default() {
        let err = validate_global_args_with_euid(
            &user_ctx(),
            system_only("adopt", DryRunPolicy::UnprivilegedPreview),
            1000,
            false,
        )
        .expect_err("system-only command should reject user mode");

        assert_eq!(err.code(), "PERMISSION_DENIED");
        assert!(err.hint().unwrap().contains("sudo anolisa adopt"));
        assert!(!err.hint().unwrap().contains("--install-mode system"));
    }

    #[test]
    fn system_only_explicit_user_mode_is_invalid_argument() {
        let err = validate_global_args_with_euid(
            &user_ctx(),
            system_only("adopt", DryRunPolicy::UnprivilegedPreview),
            1000,
            true,
        )
        .expect_err("explicit user mode is invalid for system-only commands");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("system scope"));
        assert!(err.hint().is_none());
    }

    #[test]
    fn root_system_only_user_mode_reports_scope_error() {
        let err = validate_global_args_with_euid(
            &user_ctx(),
            system_only("adopt", DryRunPolicy::UnprivilegedPreview),
            0,
            true,
        )
        .expect_err("system-only command should not use generic root user-mode guidance");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("system scope"));
        assert!(!err.reason().contains("without sudo"));
    }

    #[test]
    fn generated_help_includes_all_subcommands() {
        let mut cmd = build_cli();
        let help = cmd.render_help().to_string();

        for sub in Cli::command().get_subcommands() {
            let name = sub.get_name();
            assert!(
                help.contains(name),
                "subcommand `{name}` missing from generated help output"
            );
        }
    }

    #[test]
    fn generated_help_keeps_management_commands_in_management_section() {
        let mut cmd = build_cli();
        let help = cmd.render_help().to_string();
        let management = help
            .split("Management Commands:\n")
            .nth(1)
            .and_then(|rest| rest.split("\nOther:\n").next())
            .expect("management help section must exist");

        for sub in
            ManagementCommands::augment_subcommands(clap::Command::new("group")).get_subcommands()
        {
            let name = sub.get_name();
            assert!(
                management.contains(name),
                "management subcommand `{name}` missing from Management Commands section"
            );
        }
    }

    #[test]
    fn alias_appears_in_help() {
        let mut cmd = build_cli();
        let help = cmd.render_help().to_string();
        assert!(
            help.contains("ls"),
            "visible alias `ls` should appear in help output"
        );
    }

    #[test]
    fn install_mode_is_none_when_omitted() {
        let cmd = build_cli();
        let matches = cmd.try_get_matches_from(["anolisa", "status"]).unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        assert_eq!(cli.install_mode, None);
    }

    #[test]
    fn install_mode_is_some_when_explicitly_set() {
        let cmd = build_cli();
        let matches = cmd
            .try_get_matches_from(["anolisa", "--install-mode=system", "status"])
            .unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        assert_eq!(cli.install_mode, Some(InstallMode::System));
    }
}
