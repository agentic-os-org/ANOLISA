//! SkillFS CLI — AI agent skill management via virtual filesystem.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

fn cleanup_pid_file(pid_file: &Option<PathBuf>) {
    if let Some(p) = pid_file {
        match std::fs::remove_file(p) {
            Ok(()) => tracing::info!(path = %p.display(), "removed PID file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(path = %p.display(), error = %e, "failed to remove PID file"),
        }
    }
}

use clap::{CommandFactory, Parser, Subcommand};
use skillfs_core::store::SkillStore;
use skillfs_core::views::ViewsConfig;
use skillfs_core::{ParseConfig, SharedSkillStore};
use skillfs_fuse::security::{
    ActivationMode, ActivationReloadController, ActivationWatcher, ActiveSkillResolver,
    AuditRuntimeConfig, CliLedgerAdapter, ControlSocketConfig, ControlSocketContext,
    ControlSocketServer, DEFAULT_NOTIFY_DEBOUNCE_MS, DEFAULT_NOTIFY_TIMEOUT_MS,
    DEFAULT_RELOAD_INTERVAL_MS, DEFAULT_RELOAD_TIMEOUT_MS, DecisionCommand,
    InstallerStagingController, JsonlProtocolEventWriter, JsonlSecurityEventWriter, LedgerAdapter,
    LedgerBackingRoot, NoopProtocolEventWriter, NoopSecurityEventWriter, NotifyClient,
    NotifyController, ProtocolEventWriter, RefreshController, ReloadMode, RuntimeDecisionOutcome,
    RuntimeMetricsSink, RuntimeMetricsWriter, SecurityConfig, SecurityEventWriter,
    SecurityModeConfig, SessionStatsWriter, SkillfsSessionStats, SourceDriftObserver,
    StagingMatcher, SummaryWriteOutcome, TrustedPeerConfig, TrustedWriterConfig,
    UnixSocketNotifyClient, bootstrap_activation, resolve_events_path,
    resolve_protocol_events_path, spawn_drift_watcher,
};
use skillfs_fuse::{FuseError as FuseErr, MountConfig, MountOptions, mount_configured};
use tokio::signal;
use tracing::{debug, error, info, warn};

mod help_text;
mod managed;
mod mount_file;
mod sls_ops;

#[derive(Clone, Debug)]
struct SourceRoots {
    canonical_identity_root: PathBuf,
    physical_source_root: PathBuf,
}

impl SourceRoots {
    fn resolve(source: &Path) -> std::io::Result<Self> {
        Ok(Self {
            canonical_identity_root: lexical_absolute(source)?,
            physical_source_root: source.canonicalize()?,
        })
    }

    fn canonical_identity_root(&self) -> &Path {
        &self.canonical_identity_root
    }

    fn physical_source_root(&self) -> &Path {
        &self.physical_source_root
    }
}

fn lexical_absolute(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(name) => normalized.push(name),
        }
    }
    Ok(normalized)
}

#[derive(Clone, Debug)]
struct MountRuntimeRoots {
    notify_canonical_root: PathBuf,
    physical_source_root: PathBuf,
    daemon_root: PathBuf,
}

impl MountRuntimeRoots {
    fn from_source(source: &SourceRoots, ledger_backing_root: Option<&Path>) -> Self {
        Self {
            notify_canonical_root: source.canonical_identity_root().to_path_buf(),
            physical_source_root: source.physical_source_root().to_path_buf(),
            daemon_root: ledger_backing_root
                .unwrap_or(source.physical_source_root())
                .to_path_buf(),
        }
    }

    fn notify_canonical_root(&self) -> &Path {
        &self.notify_canonical_root
    }

    fn physical_source_root(&self) -> &Path {
        &self.physical_source_root
    }

    fn daemon_root(&self) -> &Path {
        &self.daemon_root
    }
}

fn build_notify_controller(
    client: Arc<dyn NotifyClient>,
    roots: &MountRuntimeRoots,
    notify_timeout_ms: u64,
    protocol_event_writer: Arc<dyn ProtocolEventWriter>,
    reload_controller: Option<Arc<ActivationReloadController>>,
) -> Arc<NotifyController> {
    if let Some(reload) = reload_controller {
        NotifyController::new_with_reload(
            client,
            roots.notify_canonical_root().to_path_buf(),
            roots.daemon_root().to_path_buf(),
            std::time::Duration::from_millis(DEFAULT_NOTIFY_DEBOUNCE_MS),
            notify_timeout_ms,
            protocol_event_writer,
            reload,
        )
    } else {
        NotifyController::new_with_protocol_writer(
            client,
            roots.notify_canonical_root().to_path_buf(),
            roots.daemon_root().to_path_buf(),
            std::time::Duration::from_millis(DEFAULT_NOTIFY_DEBOUNCE_MS),
            notify_timeout_ms,
            protocol_event_writer,
        )
    }
}

// ---------------------------------------------------------------------------
// CLI Arguments
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "skillfs")]
#[command(about = "Expose curated agent skills through a virtual filesystem")]
#[command(long_about = help_text::CLI_LONG_ABOUT)]
#[command(after_help = help_text::CLI_AFTER_HELP)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Write SkillFS process logs to this file instead of stderr.
    ///
    /// The filename may contain `{pid}`, for example
    /// `/tmp/skillfs-{pid}.log`. This is daemon logging, not audit JSONL.
    #[arg(long, value_name = "PATH", global = true)]
    log_file: Option<PathBuf>,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Mount the SkillFS virtual filesystem
    #[command(long_about = help_text::MOUNT_LONG_ABOUT)]
    #[command(after_help = help_text::MOUNT_AFTER_HELP)]
    Mount {
        /// Directory that stores skill folders, SKILL.md, and skillfs-views.toml.
        #[arg(value_name = "SOURCE", required_unless_present = "config", requires = "mountpoint", help_heading = help_text::HEADING_MOUNT)]
        source: Option<PathBuf>,

        /// Directory where SkillFS exposes the virtual /skills view.
        #[arg(value_name = "MOUNTPOINT", required_unless_present = "config", requires = "source", help_heading = help_text::HEADING_MOUNT)]
        mountpoint: Option<PathBuf>,

        /// Allow users other than the mounter to access the FUSE mount.
        #[arg(long, help_heading = help_text::HEADING_MOUNT)]
        allow_other: bool,

        /// Reject writes through the FUSE mount, including propagated submounts.
        ///
        /// The separate physical source remains writable by its owner.
        #[arg(long, help_heading = help_text::HEADING_MOUNT)]
        read_only: bool,

        /// Root visible to readers for paths advertised by skill-discover.
        ///
        /// Set this to the mounted skills directory when readers cannot access
        /// the physical source path, such as a separate workload container.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_MOUNT)]
        skill_discover_root: Option<PathBuf>,

        /// Keep SkillFS in the foreground; useful for tests and systemd.
        #[arg(long, help_heading = help_text::HEADING_PROCESS)]
        foreground: bool,

        /// Opt in to managed mode: keep the mount alive across gateway
        /// restarts.
        ///
        /// Starts a detached supervisor (in its own session) that holds the
        /// desired state as "mounted" and remounts the FUSE worker if it
        /// exits unexpectedly. The command returns once the mount is ready.
        /// Clear the desired state with `skillfs stop <MOUNTPOINT>`.
        #[arg(long, help_heading = help_text::HEADING_PROCESS)]
        managed: bool,

        /// Write the SkillFS process PID after the mount starts.
        ///
        /// Use `kill -TERM $(cat <file>)` to stop the foreground daemon and
        /// unmount cleanly.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_PROCESS)]
        pid_file: Option<PathBuf>,

        /// Append best-effort filesystem audit events as JSONL.
        ///
        /// Records policy decisions, link/xattr attempts, selected reads and
        /// writes, and source drift observations. If the path cannot be opened,
        /// startup fails before mounting.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_OBSERVABILITY)]
        audit_log: Option<PathBuf>,

        /// Queue size for the audit writer thread.
        ///
        /// `0` uses the built-in default. Only applies with `--audit-log`.
        #[arg(
            long,
            value_name = "N",
            default_value_t = 0,
            help_heading = help_text::HEADING_OBSERVABILITY
        )]
        audit_queue_capacity: usize,

        /// Require in-place mounting: SOURCE and MOUNTPOINT must be the same.
        ///
        /// Use this when security policy or audit must cover normal source-path
        /// access. Without it, non-in-place mounts remain allowed but direct
        /// writes to SOURCE bypass SkillFS.
        #[arg(long, help_heading = help_text::HEADING_SECURITY)]
        security_mode: bool,

        /// External command used by the legacy scan/resolve security flow.
        ///
        /// SkillFS appends `scan <skill_dir> --json` and
        /// `resolve <skill_dir> --json`. Mutually exclusive with
        /// `--activation-mode file`.
        #[arg(long, value_name = "COMMAND", help_heading = help_text::HEADING_SECURITY)]
        decision_command: Option<String>,

        /// Enable the security pipeline.
        ///
        /// Requires either `--decision-command` or `--activation-mode file`.
        /// Without this flag, mounted skills use the default passthrough view.
        #[arg(long, help_heading = help_text::HEADING_SECURITY)]
        security: bool,

        /// Write security decision events as JSONL for the legacy flow.
        ///
        /// Only applies with `--security` and `--decision-command`.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_OBSERVABILITY)]
        events_log: Option<PathBuf>,

        /// [DEPRECATED / compatibility] Trusted writer process-name
        /// gate. Matches the FUSE caller's process `comm` via
        /// `/proc/<tgid>/comm`. Process `comm` can be spoofed via
        /// `prctl(PR_SET_NAME)` or by exec'ing a same-basename
        /// binary; NOT production-strength. Use
        /// `--trusted-writer-exe` instead.
        #[arg(long, value_name = "NAME", help_heading = help_text::HEADING_TRUSTED_WRITERS)]
        trusted_writer: Option<String>,

        /// [RECOMMENDED] Trusted writer executable identity gate.
        /// Matches the FUSE caller's `/proc/<tgid>/exe` readlink
        /// against the configured canonical path and on-disk file
        /// identity `(dev, ino)`. Resistant to process-name spoofing.
        /// Requires Linux. The path must exist and be a regular file.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_TRUSTED_WRITERS)]
        trusted_writer_exe: Option<PathBuf>,

        /// TOML mount configuration (sources and mountpoint), or legacy security configuration.
        ///
        /// Mount configuration replaces positional paths; security configuration keeps CLI overrides.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_CONFIG)]
        config: Option<PathBuf>,

        /// Activation source: `off` or `file`.
        ///
        /// `file` reads `<skill_dir>/.skill-meta/activation.json` to decide
        /// whether each skill is current, served from snapshot, or hidden.
        /// Requires `--security` and is mutually exclusive with
        /// `--decision-command`.
        #[arg(long, value_name = "MODE", help_heading = help_text::HEADING_LEDGER)]
        activation_mode: Option<String>,

        /// Notify a ledger daemon after debounced FUSE mutations.
        ///
        /// SkillFS sends change notifications over this Unix socket. The
        /// daemon owns scan, reconcile, and activation refresh. Requires
        /// `--security --activation-mode file` and is mutually exclusive
        /// with `--decision-command`.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_LEDGER)]
        notify_socket: Option<PathBuf>,

        /// Shared-key file used to authenticate the SkillFS notify client.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_LEDGER)]
        notify_auth_key_file: Option<PathBuf>,

        /// Write daemon protocol events as JSONL.
        ///
        /// Records debounced FUSE mutations that should be reconciled by an
        /// external ledger daemon. Write failures warn but do not affect FUSE.
        /// Requires `--security --activation-mode file` and is mutually
        /// exclusive with `--decision-command`.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_OBSERVABILITY)]
        activation_events_log: Option<PathBuf>,

        /// Activation reload mode: `off` or `poll`.
        ///
        /// `poll` re-reads activation.json / xattr after debounced notify
        /// events and updates active skill mapping without a remount. Requires
        /// `--security --activation-mode file`.
        #[arg(long, value_name = "MODE", help_heading = help_text::HEADING_LEDGER)]
        activation_reload_mode: Option<String>,

        /// Private source-side work path for the ledger daemon.
        ///
        /// Use this with in-place security mounts so the daemon can inspect
        /// the live source tree while agents see the FUSE over-mount. SkillFS
        /// validates the path and fails closed if ownership, permissions, or
        /// mount layout are unsafe. Requires `--security --activation-mode
        /// file` and is mutually exclusive with `--decision-command`.
        ///
        /// Recommended location: `/run/user/$UID/skillfs-ledger/...` or
        /// `/run/skillfs-ledger/...`. Do NOT use `/tmp` or `/var/tmp`:
        /// agent-sec-core.service runs with PrivateTmp=true, so a daemon-facing
        /// path there is invisible to the daemon and startup is rejected.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_LEDGER)]
        ledger_backing_root: Option<PathBuf>,

        /// Unix domain socket path for the trusted peer control
        /// channel. Overrides the default per-user endpoint
        /// `/run/user/<uid>/skillfs/control.sock`. SkillFS creates a
        /// control socket at this path and accepts connections from
        /// trusted peers. Requires exactly one of `--trusted-peer-exe` or
        /// `--trusted-peer-key-file`. Linux only.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_TRUSTED_PEER)]
        control_socket: Option<PathBuf>,

        /// Trusted peer executable path for control socket
        /// authentication. The peer's `/proc/<pid>/exe` must match this
        /// canonical path and its on-disk `(dev, ino)` file identity.
        /// The path must exist and be a regular file. Enables the control
        /// plane; with no `--control-socket` it binds the default per-user
        /// endpoint `/run/user/<uid>/skillfs/control.sock`.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_TRUSTED_PEER)]
        trusted_peer_exe: Option<PathBuf>,

        /// Shared-key file for container-safe control socket authentication.
        /// Mutually exclusive with `--trusted-peer-exe`.
        #[arg(long, value_name = "PATH", help_heading = help_text::HEADING_TRUSTED_PEER)]
        trusted_peer_key_file: Option<PathBuf>,

        /// Optional trusted peer UID constraint for control
        /// socket authentication. When set, the peer's UID (from
        /// `SO_PEERCRED`) must match this value.
        #[arg(long, value_name = "UID", help_heading = help_text::HEADING_TRUSTED_PEER)]
        trusted_peer_uid: Option<u32>,

        /// Optional trusted peer GID constraint for control
        /// socket authentication. When set, the peer's GID (from
        /// `SO_PEERCRED`) must match this value.
        #[arg(long, value_name = "GID", help_heading = help_text::HEADING_TRUSTED_PEER)]
        trusted_peer_gid: Option<u32>,

        /// Skill directory layout mode.
        ///
        /// `auto` (default): detect Hermes from source-root markers
        /// (`.bundled_manifest` or `.hub/`), otherwise flat. `flat`: each
        /// top-level directory under SOURCE is a skill containing
        /// SKILL.md. `hermes`: SOURCE is a Hermes hub workspace —
        /// management paths (.hub, .bundled_manifest, .no-bundled-skills)
        /// are passthrough; skills live at category/skill/SKILL.md (and
        /// top-level skill/SKILL.md).
        ///
        /// Hermes mode supports --security --activation-mode file
        /// for nested skill activation and notify, and the read-only
        /// `skill.resolveLiveSource` control socket query. Incompatible
        /// with --decision-command.
        #[arg(long, value_name = "MODE", help_heading = help_text::HEADING_MOUNT)]
        skill_layout: Option<String>,
    },

    /// Generate or update skillfs-views.toml from a skill directory
    #[command(after_help = help_text::CLASSIFY_AFTER_HELP)]
    Classify {
        /// Source directory containing skills
        #[arg(value_name = "SOURCE")]
        source: PathBuf,

        /// Number of skills to place in the primary (default) view
        #[arg(long, default_value = "6")]
        primary_count: usize,

        /// Preview only — do not write skillfs-views.toml
        #[arg(long)]
        dry_run: bool,
    },

    /// Validate skill files
    Validate {
        /// Source directory containing skills
        #[arg(value_name = "SOURCE")]
        source: PathBuf,

        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: OutputFormat,
    },

    /// List all skills
    List {
        /// Source directory containing skills
        #[arg(value_name = "SOURCE")]
        source: PathBuf,

        /// Only show enabled skills
        #[arg(long)]
        enabled_only: bool,
    },

    /// Stop a managed mount and clear its desired state
    #[command(after_help = help_text::STOP_AFTER_HELP)]
    Stop {
        /// Mountpoint of the managed mount to stop.
        #[arg(value_name = "MOUNTPOINT")]
        mountpoint: PathBuf,
    },

    /// Internal: run the managed mount supervisor (spawned by `mount --managed`).
    #[command(hide = true)]
    Supervise {
        /// Managed instance identifier.
        #[arg(long, value_name = "ID")]
        instance: String,
    },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Capture the raw arguments (excluding the program name) before clap
    // consumes them. Managed mode reconstructs the foreground worker
    // invocation from these so every mount flag is preserved verbatim.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let cli = Cli::parse();

    let pid = std::process::id();

    // Arm the SLS ops guard before any logging output so a broken stdout /
    // stderr pipe that closes before the first write still yields exactly one
    // ops record via Drop. Stop / Supervise are internal and left unlogged.
    let sls_guard = match &cli.command {
        Commands::Mount { .. } => Some(SlsOpsGuard::new("mount")),
        Commands::Classify { .. } => Some(SlsOpsGuard::new("classify")),
        Commands::Validate { .. } => Some(SlsOpsGuard::new("validate")),
        Commands::List { .. } => Some(SlsOpsGuard::new("list")),
        Commands::Stop { .. } | Commands::Supervise { .. } => None,
    };

    let max_level = if cli.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };

    // Initialize logging — to a file if --log-file was given, otherwise stderr.
    if let Some(ref log_path_template) = cli.log_file {
        // Replace `{pid}` placeholder in the path.
        let log_path_str = log_path_template
            .to_string_lossy()
            .replace("{pid}", &pid.to_string());
        let log_path = PathBuf::from(&log_path_str);

        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(file) => {
                let subscriber = tracing_subscriber::fmt()
                    .with_max_level(max_level)
                    .with_ansi(false) // no ANSI colour codes in log files
                    .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
                    .with_writer(std::sync::Mutex::new(file))
                    .finish();
                let _ = tracing::subscriber::set_global_default(subscriber);
                // Can't use info!() yet — subscriber just set
                eprintln!("skillfs: logging to {}", log_path.display());
            }
            Err(e) => {
                // Fall back to stderr and warn.
                let subscriber = tracing_subscriber::fmt()
                    .with_max_level(max_level)
                    .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
                    .with_writer(std::io::stderr)
                    .finish();
                let _ = tracing::subscriber::set_global_default(subscriber);
                eprintln!(
                    "skillfs: failed to open log file '{}': {} — falling back to stderr",
                    log_path.display(),
                    e
                );
            }
        }
    } else {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(max_level)
            .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
            .with_writer(std::io::stderr)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    info!(pid, "starting skillfs CLI");

    if let Err(e) = run(cli, raw_args, sls_guard).await {
        error!(error = %e, "command failed");
        std::process::exit(1);
    }
}

async fn run(
    cli: Cli,
    raw_args: Vec<String>,
    guard: Option<SlsOpsGuard>,
) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Mount {
            source,
            mountpoint,
            allow_other,
            read_only,
            skill_discover_root,
            foreground,
            managed,
            pid_file,
            audit_log,
            audit_queue_capacity,
            security_mode,
            decision_command,
            security,
            events_log,
            trusted_writer,
            trusted_writer_exe,
            config,
            activation_mode,
            notify_socket,
            notify_auth_key_file,
            activation_events_log,
            activation_reload_mode,
            ledger_backing_root,
            control_socket,
            trusted_peer_exe,
            trusted_peer_key_file,
            trusted_peer_uid,
            trusted_peer_gid,
            skill_layout,
        } => {
            let inputs = (|| -> Result<_, Box<dyn std::error::Error>> {
                let mount_file = config
                    .as_deref()
                    .map(mount_file::load)
                    .transpose()?
                    .flatten();
                if mount_file.is_some() {
                    let matches = Cli::command().try_get_matches_from(
                        std::iter::once("skillfs".to_string()).chain(raw_args.iter().cloned()),
                    )?;
                    let (_, args) = matches.subcommand().ok_or("missing mount command")?;
                    mount_file::validate_options(args)?;
                }
                let inputs = if let Some(file) = mount_file {
                    let file = file.validate()?;
                    (
                        file.sources[0].clone(),
                        file.mountpoint,
                        Some(file.sources),
                        None,
                    )
                } else {
                    (
                        source.ok_or("SOURCE is required with a security configuration")?,
                        mountpoint.ok_or("MOUNTPOINT is required with a security configuration")?,
                        None,
                        config,
                    )
                };
                Ok(inputs)
            })();
            let (source, mountpoint, sources, config) = match inputs {
                Ok(inputs) => inputs,
                Err(error) => {
                    finish_sls(guard, Some(error.to_string()));
                    return Err(error);
                }
            };
            let multi_source = sources.as_ref().is_some_and(|roots| roots.len() > 1);
            let read_only = read_only || multi_source;
            let skill_discover_root = if sources.is_some() {
                Some(mountpoint.join("skills"))
            } else {
                skill_discover_root
            };
            let skill_layout = if sources.is_some() {
                Some("flat".to_string())
            } else {
                skill_layout
            };
            if let Some(root) = &skill_discover_root {
                if !root.is_absolute() {
                    let result = Err(format!(
                        "--skill-discover-root must be absolute, got '{}'",
                        root.display()
                    )
                    .into());
                    finish_sls(guard, err_reason(&result));
                    return result;
                }
            }
            if managed {
                // Managed mode: spawn a detached supervisor and return once
                // the mount is ready. The supervisor re-invokes this binary
                // as a foreground worker using the preserved raw arguments.
                // Log this public mount invocation too — the detached worker's
                // own mount record is separate.
                let result = managed::run_client(&raw_args, &source, &mountpoint);
                finish_sls(guard, err_reason(&result));
                return result;
            }
            // Log the mount startup attempt as a best-effort ops record. The
            // mount may be long-running, so this captures startup success or
            // failure; the mount-session summary writer remains untouched.
            let result = cmd_mount(
                source,
                sources,
                mountpoint,
                allow_other,
                read_only,
                skill_discover_root,
                foreground,
                pid_file,
                audit_log,
                audit_queue_capacity,
                security_mode,
                decision_command,
                security,
                events_log,
                trusted_writer,
                trusted_writer_exe,
                config,
                activation_mode,
                notify_socket,
                notify_auth_key_file,
                activation_events_log,
                activation_reload_mode,
                ledger_backing_root,
                control_socket,
                trusted_peer_exe,
                trusted_peer_key_file,
                trusted_peer_uid,
                trusted_peer_gid,
                skill_layout,
            )
            .await;
            finish_sls(guard, err_reason(&result));
            result
        }
        Commands::Classify {
            source,
            primary_count,
            dry_run,
        } => {
            let result = cmd_classify(source, primary_count, dry_run).await;
            finish_sls(guard, err_reason(&result));
            result
        }
        Commands::Validate { source, format } => {
            let (result, validation_failed) = cmd_validate(source, format).await;
            // A validation failure exits non-zero but is not a command error;
            // record it with a concise err_reason before exiting.
            let reason = match err_reason(&result) {
                Some(r) => Some(r),
                None if validation_failed => Some("validation failed".to_string()),
                None => None,
            };
            // Write now, not on drop: process::exit below skips destructors, so
            // the validation-failure record must land before the exit.
            finish_sls(guard, reason);
            if result.is_ok() && validation_failed {
                std::process::exit(1);
            }
            result
        }
        Commands::List {
            source,
            enabled_only,
        } => {
            let result = cmd_list(source, enabled_only).await;
            finish_sls(guard, err_reason(&result));
            result
        }
        Commands::Stop { mountpoint } => managed::run_stop(&mountpoint),
        Commands::Supervise { instance } => managed::run_supervisor(&instance),
    }
}

/// Extract a concise error string from a command result for the SLS ops log.
fn err_reason<T>(result: &Result<T, Box<dyn std::error::Error>>) -> Option<String> {
    result.as_ref().err().map(|e| e.to_string())
}

/// Guarantees each CLI command emits exactly one SLS ops record on every exit
/// path. It is armed in `main` before logging is initialized, so the `Drop`
/// fallback is live if tracing's internal error report panics after an early
/// EPIPE. The same fallback handles later `println!`/`eprintln!` broken-pipe
/// panics. `finish` writes the record immediately and disarms the guard; if the
/// command unwinds first, `Drop` writes a single `err_reason="panic"` record
/// instead.
///
/// `finish` writes eagerly rather than deferring to `Drop` because
/// `process::exit` (used by `validate` on validation failure) skips
/// destructors — the record must already be on disk before any explicit exit.
/// Covers panic unwinding only, not `abort`/SIGKILL, which never run drops.
struct SlsOpsGuard {
    ops_name: &'static str,
    start: std::time::Instant,
    // `true` until `finish` runs; gates the panic fallback in `Drop`.
    armed: bool,
}

impl SlsOpsGuard {
    fn new(ops_name: &'static str) -> Self {
        Self {
            ops_name,
            start: std::time::Instant::now(),
            armed: true,
        }
    }

    /// Write the ops record now and disarm the panic fallback.
    fn finish(mut self, reason: Option<String>) {
        self.armed = false;
        sls_ops::log_command(self.ops_name, self.start, reason);
    }
}

impl Drop for SlsOpsGuard {
    fn drop(&mut self) {
        if self.armed {
            // Reached only when the command unwound before `finish`.
            sls_ops::log_command(self.ops_name, self.start, Some("panic".to_string()));
        }
    }
}

/// Finish the command's SLS guard, if any, writing exactly one ops record. A
/// no-op for the internal Stop/Supervise commands, which carry no guard.
fn finish_sls(guard: Option<SlsOpsGuard>, reason: Option<String>) {
    if let Some(guard) = guard {
        guard.finish(reason);
    }
}

// ---------------------------------------------------------------------------
// Mount Command
// ---------------------------------------------------------------------------

/// Debounce window for the runtime source drift watcher (Package W1).
///
/// Mirrors the value used in the existing `skillfs-core::watcher` integration
/// tests. Drift observation is best-effort, so a few-hundred-ms coalescing
/// window keeps audit volume reasonable without losing the signal that an
/// out-of-band change happened.
const DRIFT_DEBOUNCE_MS: u64 = 200;

/// Resolve the canonical path a daemon-facing path will occupy, resolving
/// symlinks in **every** existing ancestor even when one or more trailing
/// components do not exist yet.
///
/// A plain parent-only canonicalize (used previously) resolves only the
/// direct parent, and falls back to the raw input when that parent does not
/// exist. That misses `link-to-tmp/missing/leaf`, where `link-to-tmp` is a
/// symlink into `/tmp` but `missing` does not exist yet: the raw lexical
/// path shows no `/tmp` prefix, yet the object is created under `/tmp`. This
/// walk climbs to the deepest existing ancestor, canonicalizes it (resolving
/// the whole symlink chain), and re-appends the remaining components.
///
/// Returns `None` when the path cannot be reliably resolved (no existing
/// ancestor, or an ancestor cannot be canonicalized) so the caller can fail
/// closed instead of trusting an unverified lexical prefix.
fn resolve_daemon_facing_path(p: &Path) -> Option<PathBuf> {
    let mut trailing: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = p;
    loop {
        // `exists()` follows symlinks, which is exactly what we want for
        // ancestors: a symlinked ancestor pointing at an existing directory
        // is the deepest resolvable point, and `canonicalize` resolves it.
        if cursor.exists() {
            let base = cursor.canonicalize().ok()?;
            let mut result = base;
            for name in trailing.iter().rev() {
                result.push(name);
            }
            return Some(result);
        }
        // No filename (e.g. a trailing `..`) means we cannot reason about
        // the path safely — fail closed.
        let name = cursor.file_name()?;
        trailing.push(name.to_os_string());
        match cursor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => cursor = parent,
            _ => {
                // Reached the top with no existing ancestor. For an absolute
                // path this only happens if `/` is missing; for a relative
                // path, anchor at the current directory.
                let base = Path::new(".").canonicalize().ok()?;
                let mut result = base;
                for name in trailing.iter().rev() {
                    result.push(name);
                }
                return Some(result);
            }
        }
    }
}

/// Return `true` when a daemon-facing path resolves under a private-tmp
/// root (`/tmp` or `/var/tmp`).
///
/// `agent-sec-core.service` runs with `PrivateTmp=true`, so the daemon
/// gets a private mount namespace where the host `/tmp` and `/var/tmp`
/// are invisible. A daemon-facing source/backing root under those roots
/// would make the daemon reject notifications and time out activation.
/// Both the literal and canonical forms of the tmp roots are checked so
/// a symlinked `/tmp` (e.g. `/tmp -> /private/tmp`) is still caught.
fn daemon_facing_path_under_private_tmp(candidate: &Path) -> bool {
    for root in ["/tmp", "/var/tmp"] {
        let root_path = Path::new(root);
        if candidate.starts_with(root_path) {
            return true;
        }
        if let Ok(canon_root) = root_path.canonicalize() {
            if candidate.starts_with(&canon_root) {
                return true;
            }
        }
    }
    false
}

/// Return `true` when an operator-supplied daemon-facing argument resolves
/// under a private-tmp root.
///
/// Resolution climbs to the deepest existing ancestor and canonicalizes it,
/// so an ancestor symlink into `/tmp` is caught even when trailing
/// components (e.g. a not-yet-created parent directory) do not exist. A path
/// that cannot be reliably resolved is treated as unsafe (fail closed).
fn daemon_facing_arg_under_private_tmp(path: &Path) -> bool {
    match resolve_daemon_facing_path(path) {
        Some(resolved) => daemon_facing_path_under_private_tmp(&resolved),
        None => true,
    }
}

#[allow(clippy::too_many_arguments)]
async fn cmd_mount(
    source: PathBuf,
    sources: Option<Vec<PathBuf>>,
    mountpoint: PathBuf,
    allow_other: bool,
    read_only: bool,
    skill_discover_root: Option<PathBuf>,
    foreground: bool,
    pid_file: Option<PathBuf>,
    audit_log: Option<PathBuf>,
    audit_queue_capacity: usize,
    security_mode: bool,
    decision_command: Option<String>,
    security: bool,
    events_log: Option<PathBuf>,
    trusted_writer: Option<String>,
    trusted_writer_exe: Option<PathBuf>,
    config_path: Option<PathBuf>,
    activation_mode_raw: Option<String>,
    notify_socket: Option<PathBuf>,
    notify_auth_key_file: Option<PathBuf>,
    activation_events_log: Option<PathBuf>,
    activation_reload_mode_raw: Option<String>,
    ledger_backing_root: Option<PathBuf>,
    control_socket: Option<PathBuf>,
    trusted_peer_exe: Option<PathBuf>,
    trusted_peer_key_file: Option<PathBuf>,
    trusted_peer_uid: Option<u32>,
    trusted_peer_gid: Option<u32>,
    skill_layout_raw: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(source = %source.display(), mountpoint = %mountpoint.display(), security_mode, "mounting SkillFS");

    // Load TOML config if --config is set. CLI flags override config values.
    let file_config = match config_path {
        Some(ref p) => {
            let cfg = SecurityConfig::load(p)
                .map_err(|e| format!("failed to load config '{}': {e}", p.display()))?;
            info!(path = %p.display(), "loaded security config");
            Some(cfg)
        }
        None => None,
    };

    // Build the opt-in OS adapter transform stage from
    // `[transforms.os_adapter]`. Loading, YAML/schema validation, and (for
    // `target_os = "auto"`) `/etc/os-release` detection all happen here, before
    // the mount begins, so a missing/invalid rule artifact or an unrecognized
    // auto target OS produces an actionable startup error instead of a silently
    // disabled adapter. `None` config keeps the default directive-only pipeline.
    let os_adapter_stage = match file_config.as_ref() {
        Some(cfg) => cfg.build_os_adapter_stage().map_err(|e| format!("{e}"))?,
        None => None,
    };

    // Directive/compiler stage toggle. `None` (no config) keeps the default
    // (enabled); a config file supplies the resolved value, which defaults to
    // enabled unless `[transforms.directive] enabled = false` is set.
    let directive_enabled: Option<bool> = file_config.as_ref().map(|cfg| cfg.directive_enabled());

    // Parse activation mode: CLI flag (if present) overrides config file.
    let activation_mode = match activation_mode_raw.as_deref() {
        Some(raw) => ActivationMode::parse(raw)
            .ok_or_else(|| format!("invalid --activation-mode '{raw}'; allowed: off, file"))?,
        None => file_config
            .as_ref()
            .map(|c| c.activation_mode())
            .unwrap_or_default(),
    };

    // Parse skill layout: CLI flag overrides config file; the default is
    // `auto`, which conservatively detects Hermes from source-root markers
    // (`.bundled_manifest` / `.hub/`) and otherwise falls back to flat.
    // Explicit `flat` / `hermes` always win over detection.
    let layout_intent = skill_layout_raw
        .as_deref()
        .or_else(|| file_config.as_ref().and_then(|c| c.skills_layout()));
    let skill_layout = match layout_intent {
        Some("flat") => Some(skillfs_fuse::SkillLayout::Flat),
        Some("hermes") => Some(skillfs_fuse::SkillLayout::Hermes),
        Some("auto") | None => Some(skillfs_fuse::detect_skill_layout(&source)),
        Some(other) => {
            return Err(
                format!("invalid --skill-layout '{other}'; allowed: auto, flat, hermes").into(),
            );
        }
    };

    // Parse reload mode: CLI flag (if present) overrides config file.
    let reload_mode = match activation_reload_mode_raw.as_deref() {
        Some(raw) => ReloadMode::parse(raw).ok_or_else(|| {
            format!("invalid --activation-reload-mode '{raw}'; allowed: off, poll")
        })?,
        None => file_config
            .as_ref()
            .map(|c| c.reload_mode())
            .unwrap_or_default(),
    };
    let reload_interval_ms = file_config
        .as_ref()
        .and_then(|c| c.reload_interval_ms())
        .unwrap_or(DEFAULT_RELOAD_INTERVAL_MS);
    let reload_timeout_ms = file_config
        .as_ref()
        .and_then(|c| c.reload_timeout_ms())
        .unwrap_or(DEFAULT_RELOAD_TIMEOUT_MS);
    let watcher_interval_ms = file_config
        .as_ref()
        .and_then(|c| c.watcher_interval_ms())
        .unwrap_or(skillfs_fuse::security::DEFAULT_WATCHER_INTERVAL_MS);

    // Merge: CLI flag overrides config file value.
    let decision_command = decision_command.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.decision_command().map(String::from))
    });
    let events_log = events_log.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.events_log_path().map(PathBuf::from))
    });
    let trusted_writer = trusted_writer.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.trusted_writer_name().map(String::from))
    });
    let audit_log = audit_log.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.audit_log_path().map(PathBuf::from))
    });
    let audit_queue_capacity = if audit_queue_capacity != 0 {
        audit_queue_capacity
    } else {
        file_config
            .as_ref()
            .and_then(|c| c.audit_queue_capacity())
            .unwrap_or(0)
    };

    let parsed_decision_command: Option<DecisionCommand> = match decision_command.as_deref() {
        Some(raw) => {
            let cmd = DecisionCommand::parse(raw)
                .map_err(|e| format!("invalid --decision-command '{raw}': {e}"))?;
            Some(cmd)
        }
        None => None,
    };

    // Hermes + decision-command gate (post config merge).
    //
    // The external decision protocol does not accept slash-containing
    // skill ids, so hermes nested skills cannot use --decision-command.
    // This gate runs after both CLI and config-file values are merged.
    if skill_layout == Some(skillfs_fuse::SkillLayout::Hermes) && parsed_decision_command.is_some()
    {
        return Err("--skill-layout hermes is incompatible with \
                 --decision-command (the external decision protocol \
                 does not accept slash-containing skill ids)"
            .into());
    }

    // Activation source validation:
    //   --security + --decision-command           => scan -> resolve path
    //   --security + --activation-mode file        => activation.json consumer
    //   --security + both                          => startup error (dual source)
    //   --activation-mode file without --security  => startup error
    //   --security without either source           => startup error
    if activation_mode == ActivationMode::File && !security {
        return Err("--activation-mode file requires --security".into());
    }
    if activation_mode == ActivationMode::File && parsed_decision_command.is_some() {
        return Err(
            "--activation-mode file and --decision-command are mutually exclusive \
             (activation.json and scan->resolve cannot both populate the resolver)"
                .into(),
        );
    }

    // ── Trusted peer control socket: merge CLI over config ──────────
    //
    // Merge CLI flags with the config file (CLI overrides config) here,
    // BEFORE any control-plane validation, so the mutual-requirement,
    // semantic, endpoint-classification, and backing-root checks all see a
    // single merged view. This is the only place `control_socket` /
    // `trusted_peer_exe` are resolved; there is no second merge later.
    let control_socket = control_socket.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.control_socket_path().map(PathBuf::from))
    });
    let config_peer_exe = || {
        file_config
            .as_ref()
            .and_then(|c| c.control_socket_trusted_peer_exe().map(PathBuf::from))
    };
    let config_peer_key = || {
        file_config
            .as_ref()
            .and_then(|c| c.control_socket_trusted_peer_key_file().map(PathBuf::from))
    };
    // An authentication selector supplied on the CLI overrides the selector
    // in the config as one unit; otherwise a CLI key plus config exe would
    // accidentally enable two modes instead of applying normal precedence.
    let (trusted_peer_exe, trusted_peer_key_file) = match (trusted_peer_exe, trusted_peer_key_file)
    {
        (Some(exe), None) => (Some(exe), None),
        (None, Some(key)) => (None, Some(key)),
        (Some(exe), Some(key)) => (Some(exe), Some(key)),
        (None, None) => (config_peer_exe(), config_peer_key()),
    };
    let trusted_peer_uid = trusted_peer_uid.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.control_socket_trusted_peer_uid())
    });
    let trusted_peer_gid = trusted_peer_gid.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.control_socket_trusted_peer_gid())
    });

    // Control socket gates — mutual requirement first, then semantic
    // gates. Must fire before the generic security source check so the
    // error message names the actual problem. All operate on the merged
    // values above.
    //
    // A trusted peer without an explicit socket path is valid: the
    // control plane binds the default per-user endpoint (resolved below).
    // Only an explicit socket path without a trusted peer is an error —
    // the control plane is always authenticated.
    if trusted_peer_exe.is_some() && trusted_peer_key_file.is_some() {
        return Err("--trusted-peer-exe and --trusted-peer-key-file are mutually exclusive".into());
    }
    if trusted_peer_key_file.is_some() && control_socket.is_none() {
        return Err("--trusted-peer-key-file requires an explicit --control-socket path".into());
    }
    let has_trusted_peer = trusted_peer_exe.is_some() || trusted_peer_key_file.is_some();
    if control_socket.is_some() && !has_trusted_peer {
        let p = control_socket.as_ref().expect("presence checked above");
        return Err(format!(
            "--control-socket {} requires a trusted peer authentication mode; \
             configure exactly one of --trusted-peer-exe or --trusted-peer-key-file",
            p.display()
        )
        .into());
    }
    // The control plane is enabled by either an explicit socket path or a
    // trusted peer (which selects the default endpoint).
    let control_plane_enabled = control_socket.is_some() || has_trusted_peer;
    if control_plane_enabled {
        if !security {
            return Err("--control-socket requires --security (the control socket \
                 writes activation state through the active resolver)"
                .into());
        }
        if activation_mode != ActivationMode::File {
            return Err("--control-socket requires --activation-mode file (the \
                 control socket writes activation files consumed by the \
                 file-based activation path)"
                .into());
        }
        if parsed_decision_command.is_some() {
            return Err("control socket and --decision-command are mutually \
                 exclusive (control socket is the daemon-driven activation \
                 path; --decision-command is the CLI-driven refresh path)"
                .into());
        }
    }

    if security && activation_mode == ActivationMode::Off && parsed_decision_command.is_none() {
        return Err(
            "--security requires --decision-command <COMMAND> or --activation-mode file".into(),
        );
    }

    // Reload mode validation.
    if reload_mode == ReloadMode::Poll {
        if !security {
            return Err("--activation-reload-mode poll requires --security".into());
        }
        if activation_mode != ActivationMode::File {
            return Err("--activation-reload-mode poll requires --activation-mode file".into());
        }
    }

    // Merge notify socket: CLI flag overrides config file.
    let notify_socket = notify_socket.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.notify_socket_path().map(PathBuf::from))
    });
    let notify_auth_key_file = notify_auth_key_file.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.notify_auth_key_file().map(PathBuf::from))
    });
    let notify_timeout_ms = file_config
        .as_ref()
        .and_then(|c| c.notify_timeout_ms())
        .unwrap_or(DEFAULT_NOTIFY_TIMEOUT_MS);

    // --notify-socket startup validation.
    if let Some(ref p) = notify_socket {
        if p.as_os_str().is_empty() {
            return Err("--notify-socket path must not be empty".into());
        }
        if !security {
            return Err(format!("--notify-socket {} requires --security", p.display()).into());
        }
        if activation_mode != ActivationMode::File {
            return Err(format!(
                "--notify-socket {} requires --activation-mode file",
                p.display()
            )
            .into());
        }
        if parsed_decision_command.is_some() {
            return Err(
                "--notify-socket and --decision-command are mutually exclusive \
                 (notify is for the daemon-driven activation path; \
                 decision-command has its own scan->resolve refresh)"
                    .into(),
            );
        }
    }
    if notify_auth_key_file.is_some() && notify_socket.is_none() {
        return Err("--notify-auth-key-file requires --notify-socket".into());
    }
    if trusted_peer_key_file.is_some() && notify_socket.is_some() && notify_auth_key_file.is_none()
    {
        return Err(
            "container HMAC control mode requires --notify-auth-key-file when --notify-socket is enabled"
                .into(),
        );
    }

    // Merge activation-events-log: CLI flag overrides config file.
    let activation_events_log = activation_events_log.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.activation_events_log_path().map(PathBuf::from))
    });

    // --activation-events-log startup validation.
    if let Some(ref p) = activation_events_log {
        if p.as_os_str().is_empty() {
            return Err("--activation-events-log path must not be empty".into());
        }
        if !security {
            return Err(format!(
                "--activation-events-log {} requires --security",
                p.display()
            )
            .into());
        }
        if activation_mode != ActivationMode::File {
            return Err(format!(
                "--activation-events-log {} requires --activation-mode file",
                p.display()
            )
            .into());
        }
        if parsed_decision_command.is_some() {
            return Err(
                "--activation-events-log and --decision-command are mutually exclusive \
                 (activation-events-log is for the daemon-driven activation path; \
                 decision-command has its own events-log)"
                    .into(),
            );
        }
        match resolve_protocol_events_path(p) {
            Ok(_) => {}
            Err(e) => {
                return Err(format!(
                    "invalid --activation-events-log path '{}': {}",
                    p.display(),
                    e
                )
                .into());
            }
        }
    }

    // P1 gate: reload=poll requires a notify trigger source. Without
    // --notify-socket or --activation-events-log the NotifyController is
    // never created, so FUSE mutations would never trigger the reload
    // poll — the operator would think reload is active while it is inert.
    if reload_mode == ReloadMode::Poll && notify_socket.is_none() && activation_events_log.is_none()
    {
        return Err("--activation-reload-mode poll requires --notify-socket or \
             --activation-events-log (without a notify trigger source, \
             reload would never fire)"
            .into());
    }

    // I2 gate: staging patterns require a notify source. Without
    // --notify-socket or --activation-events-log the NotifyController is
    // never created, so a staging rename would silently fail to emit the
    // mutation notification.
    let has_staging_patterns = file_config
        .as_ref()
        .and_then(|c| c.install.as_ref())
        .and_then(|i| i.staging_patterns.as_ref())
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    if has_staging_patterns && notify_socket.is_none() && activation_events_log.is_none() {
        return Err("install.staging_patterns requires --notify-socket or \
             --activation-events-log (without a notify source, \
             mutation notifications cannot be delivered)"
            .into());
    }

    // A6/B1: Merge ledger backing root: CLI flag overrides config file.
    let ledger_backing_root = ledger_backing_root.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.ledger_backing_root().map(PathBuf::from))
    });

    // A6/B1: --ledger-backing-root startup validation.
    if let Some(ref p) = ledger_backing_root {
        if p.as_os_str().is_empty() {
            return Err("--ledger-backing-root path must not be empty".into());
        }
        if !security {
            return Err(
                format!("--ledger-backing-root {} requires --security", p.display()).into(),
            );
        }
        if activation_mode != ActivationMode::File {
            return Err(format!(
                "--ledger-backing-root {} requires --activation-mode file",
                p.display()
            )
            .into());
        }
        if parsed_decision_command.is_some() {
            return Err(
                "--ledger-backing-root and --decision-command are mutually exclusive \
                 (backing root is for the daemon-driven activation path)"
                    .into(),
            );
        }
    }

    if security || parsed_decision_command.is_some() || events_log.is_some() {
        info!(
            security,
            decision_command = ?parsed_decision_command.as_ref().map(|c| {
                let mut s = c.program().display().to_string();
                for a in c.fixed_args() {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }),
            events_log = ?events_log.as_ref().map(|p| p.display().to_string()),
            "security mode: active resolver + refresh controller enabled"
        );
    }

    // `--events-log` is only meaningful in security mode. Surface a clear
    // startup error otherwise so an operator typo cannot silently
    // discard events. Security mode + a path that cannot be resolved (e.g.
    // missing parent dir) is also a startup error: the mount must not
    // begin without the event sink the operator asked for.
    if let Some(ref p) = events_log {
        if !security {
            return Err(format!("--events-log {} requires --security", p.display()).into());
        }
        if activation_mode == ActivationMode::File {
            return Err(format!(
                "--events-log {} is not supported with --activation-mode file \
                     (events log requires --decision-command refresh; \
                     activation event logging is a later package)",
                p.display()
            )
            .into());
        }
        match resolve_events_path(p) {
            Ok(_) => {}
            Err(e) => {
                return Err(format!("invalid --events-log path '{}': {}", p.display(), e).into());
            }
        }
    }

    // Validate source directory
    if !source.exists() {
        return Err(format!("Source directory does not exist: {}", source.display()).into());
    }
    if !source.is_dir() {
        return Err(format!("Source is not a directory: {}", source.display()).into());
    }

    // Package M0 security-mode gate. Ordered intentionally as the first
    // startup gate after the source check (and before any audit setup or
    // mountpoint auto-creation): when `--security-mode` is set, refuse to
    // mount unless `source` and `mountpoint` canonicalize to the same
    // directory. This is the only configuration in which SkillFS can
    // intercept *every* read/write to the physical source path.
    //
    // Putting this first matches the runtime fixture (validate →
    // build_sink → mount) used by the M0 integration tests and avoids
    // leaving startup side-effects (audit log file, auto-created
    // mountpoint directory) behind when the gate rejects the mount. In
    // compat mode (`--security-mode` not set) `validate()` is a no-op, so
    // the existing auto-create-mountpoint UX below is unchanged.
    let security_config = SecurityModeConfig {
        enabled: security_mode,
    };
    security_config
        .validate(&source, &mountpoint)
        .map_err(|e| format!("{}", e))?;

    // Keep external identity separate from physical I/O. The canonical
    // identity is absolute and lexically normalized without following a
    // source symlink; the physical root resolves that symlink for mount,
    // backing-root, safety, and live-source operations.
    let source_roots = SourceRoots::resolve(&source)
        .map_err(|e| format!("failed to resolve source root '{}': {e}", source.display()))?;

    // Compute mount identity before side-effecting setup. Notify v2 carries
    // canonical identity only, so the daemon needs the authenticated resolver
    // whenever the live root differs by contract: an in-place mount hides the
    // physical source, while an explicit backing root replaces the canonical
    // host path as the daemon-facing source.
    let mount_canon = mountpoint
        .canonicalize()
        .unwrap_or_else(|_| mountpoint.clone());
    let in_place = source_roots.physical_source_root() == mount_canon;
    let notify_requires_resolver =
        notify_socket.is_some() && (in_place || ledger_backing_root.is_some());
    if notify_requires_resolver && !control_plane_enabled {
        return Err(
            "--notify-socket with an in-place mount or --ledger-backing-root requires the \
             authenticated live-source resolver; \
             configure --trusted-peer-exe or the container HMAC peer mode so the daemon \
             can resolve canonicalSkillDir before accessing the source"
                .into(),
        );
    }

    // Build the runtime audit configuration. When `--audit-log` is omitted
    // the default `NoopEventSink` is preserved (Ok(None) below). When it is
    // present but the file cannot be opened, surface a startup error and
    // refuse to mount rather than silently downgrading — operators who ask
    // for audit logging must not be left without it.
    let audit_runtime = AuditRuntimeConfig {
        path: audit_log.clone(),
        queue_capacity: audit_queue_capacity,
    };
    // Package W1 safety gate. Refuse to start if `--audit-log` would land
    // inside the source tree: every audit write would either trigger the
    // drift watcher (creating a `source_changed` feedback loop on each
    // line) or land on top of an actual `<source>/<skill>/SKILL.md`,
    // corrupting the manifest SkillFS is meant to protect. The check is
    // ordered before `build_sink` so a rejected configuration never
    // creates the audit log file on disk. Disabled audit configs always
    // pass.
    audit_runtime
        .validate_audit_path_outside_source(source_roots.physical_source_root())
        .map_err(|e| format!("{}", e))?;
    // N3 source-tree guard: reject --activation-events-log inside source,
    // same rationale as audit. Ordered before the file is opened so a
    // rejected path never creates the log file on disk.
    if let Some(ref p) = activation_events_log {
        skillfs_fuse::security::validate_protocol_events_path_outside_source(
            p,
            source_roots.physical_source_root(),
        )
        .map_err(|e| format!("{}", e))?;
    }

    // #1262 PrivateTmp gate. When a daemon-facing operation is enabled,
    // agent-sec-core.service runs with PrivateTmp=true and therefore
    // cannot see the host /tmp or /var/tmp. Any daemon-facing path under
    // those roots makes the daemon reject notify, fail to tail the events
    // log, time out the activation reload, or be unable to reach the
    // control socket / open the live source the resolver reports — all of
    // which hide the affected skills. Fail fast here — before the audit
    // sink is opened, the mountpoint is auto-created, or any backing root
    // bind mount runs — so a rejected config leaves no side effects behind.
    // The pure inside-source guards above keep their own error ordering;
    // this only adds a check, never a side effect.
    //
    // This gate protects both the daemon-driven activation transport
    // (notify / events log) and the resolver control plane (the control
    // socket transport and the live source it resolves).
    //
    // Guarded paths (all daemon-facing):
    //   * backing root, or the source fallback (the daemon's scan target
    //     and the resolver's live root);
    //   * --activation-events-log (the daemon tails this JSONL file);
    //   * --notify-socket (the daemon owns this Unix socket);
    //   * --control-socket (the daemon connects to this Unix socket; the
    //     default /run/user/<uid>/... endpoint is always daemon-visible and
    //     is therefore not checked here).
    // A plain agent-visible mountpoint under /tmp is NOT guarded.
    let daemon_facing_ops = security
        && activation_mode == ActivationMode::File
        && (notify_socket.is_some() || activation_events_log.is_some() || control_plane_enabled);
    if daemon_facing_ops {
        // Daemon-facing root: the backing root when set, otherwise the
        // source is what the daemon scans directly.
        if let Some(ref br_path) = ledger_backing_root {
            if daemon_facing_arg_under_private_tmp(br_path) {
                return Err(format!(
                    "--ledger-backing-root {} resolves under /tmp or /var/tmp, which the \
                     agent-sec-core.service daemon cannot see because it runs with \
                     PrivateTmp=true. Daemon-driven activation would be rejected and the \
                     affected skills hidden. Use a daemon-visible backing root such as \
                     /run/user/$UID/skillfs-ledger/... or /run/skillfs-ledger/... instead.",
                    br_path.display()
                )
                .into());
            }
        } else if daemon_facing_path_under_private_tmp(source_roots.physical_source_root()) {
            return Err(format!(
                "the daemon-facing source root {} resolves under /tmp or /var/tmp, which the \
                 agent-sec-core.service daemon cannot see because it runs with PrivateTmp=true. \
                 Daemon-driven activation would be rejected and the affected skills hidden. \
                 Set --ledger-backing-root to a daemon-visible path such as \
                 /run/user/$UID/skillfs-ledger/... or /run/skillfs-ledger/... instead.",
                source_roots.physical_source_root().display()
            )
            .into());
        }

        // Daemon-facing transport paths. These are independent of the
        // backing root: the daemon must reach them regardless of where the
        // scan target lives.
        if let Some(ref p) = activation_events_log {
            if daemon_facing_arg_under_private_tmp(p) {
                return Err(format!(
                    "--activation-events-log {} resolves under /tmp or /var/tmp, which the \
                     agent-sec-core.service daemon cannot see because it runs with \
                     PrivateTmp=true. The daemon tails this JSONL file, so daemon-driven \
                     activation would break and the affected skills be hidden. Use a \
                     daemon-visible path such as /run/user/$UID/skillfs-ledger/... or \
                     /run/skillfs-ledger/... instead.",
                    p.display()
                )
                .into());
            }
        }
        if let Some(ref p) = notify_socket {
            if daemon_facing_arg_under_private_tmp(p) {
                return Err(format!(
                    "--notify-socket {} resolves under /tmp or /var/tmp, which the \
                     agent-sec-core.service daemon cannot see because it runs with \
                     PrivateTmp=true. The daemon owns this socket, so daemon-driven \
                     activation would break and the affected skills be hidden. Use a \
                     daemon-visible path such as /run/user/$UID/skillfs-ledger/... or \
                     /run/skillfs-ledger/... instead.",
                    p.display()
                )
                .into());
            }
        }
        // Explicit control socket. The daemon connects to this socket to
        // issue resolver queries, so a path under /tmp or /var/tmp is
        // invisible to it. Only an explicit path is checked: when the
        // control plane uses the default endpoint (`control_socket` is
        // None) the path is always under /run/user/<uid> and daemon-visible.
        if let Some(ref p) = control_socket {
            if daemon_facing_arg_under_private_tmp(p) {
                return Err(format!(
                    "--control-socket {} resolves under /tmp or /var/tmp, which the \
                     agent-sec-core.service daemon cannot see because it runs with \
                     PrivateTmp=true. The daemon connects to this socket, so the resolver \
                     control plane would be unreachable. Use the default \
                     /run/user/$UID/skillfs/control.sock endpoint (omit --control-socket) \
                     or another daemon-visible /run/... path instead.",
                    p.display()
                )
                .into());
            }
        }
    }

    let audit_sink = audit_runtime.build_sink().map_err(|e| {
        format!(
            "failed to open audit log '{}': {}",
            audit_log
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            e
        )
    })?;
    if let Some(ref p) = audit_log {
        info!(
            path = %p.display(),
            queue_capacity = audit_runtime.effective_queue_capacity(),
            "audit logging enabled"
        );
    }

    // Validate mount point. Auto-create is intentionally still here, after
    // the security-mode gate: under `--security-mode` the mountpoint must
    // already equal the source (which was checked above), so this branch
    // only ever runs in compat mode where a fresh dedicated mountpoint is
    // the expected ergonomic.
    if !mountpoint.exists() {
        info!("creating mount point directory");
        std::fs::create_dir_all(&mountpoint)?;
    }
    if !mountpoint.is_dir() {
        return Err(format!("Mount point is not a directory: {}", mountpoint.display()).into());
    }

    // A6/B1: Ledger backing root setup.
    //
    // When the operator provides --ledger-backing-root, SkillFS creates a
    // private source alias (bind mount) before the FUSE over-mount becomes
    // active. Daemon live-source operations and N3 protocol events then use
    // the backing root path; socket notify v2 keeps canonical source identity.
    // Fail-closed: unsafe backing root rejects startup.
    let backing_root: Option<LedgerBackingRoot> = if let Some(ref br_path) = ledger_backing_root {
        let br = LedgerBackingRoot::setup(
            source_roots.physical_source_root(),
            br_path,
            &mount_canon,
            in_place,
        )
        .map_err(|e| format!("--ledger-backing-root setup failed: {e}"))?;
        info!(
            backing_root = %br.path().display(),
            in_place,
            "ledger backing root enabled for live-source operations"
        );
        Some(br)
    } else {
        None
    };

    // In-place mount with daemon-facing operations requires a backing root.
    // Without it, daemon_root would fall back to source which becomes the
    // FUSE over-mount path — the daemon cannot scan through FUSE.
    //
    // The control plane (control socket / resolver) is a daemon-facing
    // operation too: `skill.resolveLiveSource` must open the physical live
    // source, not the FUSE current/fallback/hidden view. `control_plane_enabled`
    // was computed above from the merged CLI+config values.
    let has_daemon_ops = notify_socket.is_some()
        || activation_events_log.is_some()
        || reload_mode == ReloadMode::Poll
        || control_plane_enabled;
    if in_place
        && security
        && activation_mode == ActivationMode::File
        && has_daemon_ops
        && backing_root.is_none()
    {
        return Err(
            "in-place mount with activation/notify/control-socket requires \
             --ledger-backing-root (the FUSE over-mount makes the source path \
             inaccessible to the daemon and the resolver)"
                .into(),
        );
    }

    // Select the path contracts once for all later production wiring. Socket
    // notify v2 keeps canonical source identity, while daemon live-source
    // operations and N3 protocol events use the backing root when configured.
    let runtime_roots = MountRuntimeRoots::from_source(
        &source_roots,
        backing_root.as_ref().map(LedgerBackingRoot::path),
    );
    let daemon_root = runtime_roots.daemon_root().to_path_buf();

    // Load skills into store
    info!("loading skills from source directory");
    let mut store = SkillStore::new();
    let config = ParseConfig::default();
    let errors = if let Some(roots) = &sources {
        store = mount_file::load_sources(roots, &config)?;
        Vec::new()
    } else {
        store.load_from_directory(runtime_roots.physical_source_root(), &config)
    };

    if !errors.is_empty() {
        warn!(count = errors.len(), "some skills failed to load");
        for err in &errors {
            warn!(path = %err.path.display(), error = %err.error, "load error");
        }
    }

    info!(count = store.len(), "skills loaded");

    // Auto-assign any skills that are not yet in any view to the default view.
    if let Some(mut views) = ViewsConfig::load(runtime_roots.physical_source_root())
        .filter(|_| !(sources.is_some() && read_only))
    {
        let assigned = views.all_assigned_skills();
        let new_skills: Vec<String> = store
            .list()
            .iter()
            .filter(|name| !assigned.contains(**name))
            .map(|s| s.to_string())
            .collect();
        if !new_skills.is_empty() {
            info!(
                count = new_skills.len(),
                "auto-assigning new skills to default view"
            );
            if let Err(e) =
                views.assign_to_default(runtime_roots.physical_source_root(), &new_skills)
            {
                warn!(error = %e, "failed to save updated views config");
            }
        }
    }

    let shared_store: SharedSkillStore = Arc::new(parking_lot::RwLock::new(store));

    // D1.3.1 active-mapping bootstrap (read-only).
    //
    // Only fires when **both** `--security` and
    // `--decision-command` are set (both gates were checked up-front).
    // We build a fresh `ActiveSkillResolver` rooted at `source` and run
    // `scan` then `resolve` against the decision provider per skill;
    // the parsed resolve result is installed into the resolver.
    //
    // Behavior on individual failures is intentionally non-fatal:
    //  * scan failure: we skip resolve, leave the skill out of the
    //    resolver (read paths default to hidden (no activation)), and log.
    //  * resolve spawn / non-zero-exit / JSON parse errors log a
    //    warning and leave the skill out of the resolver.
    //  * a successful resolve whose `decision` cannot be installed
    //    (e.g. an empty source root) logs a warning and the skill
    //    stays hidden — same fallback as a failed resolve.
    //
    // D1.3.1 explicitly does **not** wire watcher hot sync, daemon
    // transport, or `check`/`certify`. Skill-discover is exempt from
    // the gate inside SkillFS itself, so we deliberately do not
    // run scan/resolve on it.
    let active_resolver: Option<Arc<ActiveSkillResolver>> = if security
        && activation_mode == ActivationMode::File
    {
        // A1: Activation File Consumer bootstrap.
        //
        // When `--activation-mode file` is set, SkillFS reads
        // `<skill_dir>/.skill-meta/activation.json` for every loaded
        // skill at startup and populates the resolver. Invalid or
        // missing activation files map to hidden (fail-safe).
        let resolver = ActiveSkillResolver::new(runtime_roots.physical_source_root().to_path_buf());
        let skill_names: Vec<String> = if skill_layout == Some(skillfs_fuse::SkillLayout::Hermes) {
            skillfs_fuse::security::enumerate_hermes_skill_ids(&daemon_root)
        } else {
            shared_store
                .read()
                .list()
                .iter()
                .filter(|n| **n != "skill-discover")
                .map(|s| s.to_string())
                .collect()
        };
        info!(
            count = skill_names.len(),
            activation_mode = %activation_mode,
            layout = ?skill_layout,
            "activation: loading activation files for skill mapping"
        );
        let results = bootstrap_activation(daemon_root.as_path(), &skill_names, &resolver);
        for (name, outcome) in &results {
            match outcome {
                Ok(target) => {
                    info!(
                        skill = %name,
                        target = %target.as_label(),
                        "activation file loaded"
                    );
                }
                Err(e) => {
                    warn!(
                        skill = %name,
                        error = %e,
                        "activation file invalid or missing; skill hidden (fail-safe)"
                    );
                }
            }
        }
        Some(Arc::new(resolver))
    } else if security && parsed_decision_command.is_some() {
        // D1.3.1 active-mapping bootstrap (decision-command path).
        let cmd = parsed_decision_command
            .as_ref()
            .expect("decision_command presence checked above")
            .clone();
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(CliLedgerAdapter::new(cmd.clone()));
        let resolver = ActiveSkillResolver::new(runtime_roots.physical_source_root().to_path_buf());
        let skill_names: Vec<String> = shared_store
            .read()
            .list()
            .iter()
            .filter(|n| **n != "skill-discover")
            .map(|s| s.to_string())
            .collect();
        info!(
            count = skill_names.len(),
            program = %cmd.program().display(),
            "security: resolving active skill mapping via scan -> resolve"
        );
        for name in &skill_names {
            let skill_dir = runtime_roots.physical_source_root().join(name);
            if let Err(e) = adapter.scan(&skill_dir) {
                warn!(
                    skill = %name,
                    error = %e,
                    "decision-command scan failed; skill will be hidden (no activation)"
                );
                continue;
            }
            match adapter.resolve(&skill_dir) {
                Ok(result) => match resolver.set_from_resolve_for_expected(name, &result) {
                    Ok(target) => {
                        info!(
                            skill = %name,
                            target = %target.as_label(),
                            "decision-command resolve installed"
                        );
                    }
                    Err(e) => warn!(
                        skill = %name,
                        error = %e,
                        "could not install resolve target; skill will be hidden (no activation)"
                    ),
                },
                Err(e) => warn!(
                    skill = %name,
                    error = %e,
                    "decision-command resolve failed; skill will be hidden (no activation)"
                ),
            }
        }
        Some(Arc::new(resolver))
    } else {
        None
    };

    // D1.3.1 refresh controller bootstrap.
    //
    // Only wired when `--security` and `--decision-command`
    // are both set AND we successfully built an active resolver above.
    // The controller takes the same adapter the read-path bootstrap
    // used and runs scan -> resolve on its own worker after a
    // per-skill debounce.
    //
    // `--events-log` selects the JSONL sink; its absence keeps the
    // [`NoopSecurityEventWriter`]. A `--events-log` path that cannot be
    // opened is a startup error: the operator asked for the security
    // event stream and we refuse to silently downgrade.
    let refresh_controller: Option<Arc<RefreshController>> = if security
        && active_resolver.is_some()
        && parsed_decision_command.is_some()
    {
        let cmd = parsed_decision_command
            .as_ref()
            .expect("decision_command presence checked above")
            .clone();
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(CliLedgerAdapter::new(cmd));
        let resolver_for_ctrl = active_resolver
            .clone()
            .expect("active_resolver presence checked above");
        let event_writer: Arc<dyn SecurityEventWriter> = if let Some(p) = events_log.as_ref() {
            let writer = JsonlSecurityEventWriter::new(p, 0).map_err(|e| {
                format!("failed to open --events-log path '{}': {}", p.display(), e)
            })?;
            info!(path = %p.display(), "security events JSONL enabled");
            Arc::new(writer) as Arc<dyn SecurityEventWriter>
        } else {
            Arc::new(NoopSecurityEventWriter) as Arc<dyn SecurityEventWriter>
        };
        let failed_behavior = file_config
            .as_ref()
            .map(|c| c.failed_resolve_behavior())
            .unwrap_or_default();
        let ctrl = RefreshController::new(
            adapter,
            resolver_for_ctrl,
            event_writer,
            std::time::Duration::from_millis(skillfs_fuse::security::DEFAULT_REFRESH_DEBOUNCE_MS),
            failed_behavior,
        );
        info!("security: refresh controller wired (scan -> resolve)");
        Some(ctrl)
    } else {
        // Security mode without a decision-command cannot run scans /
        // resolves; the up-front gate already errored out. Without
        // security mode at all, nothing wires the controller and the
        // mount falls back to the pre-security behavior.
        None
    };

    // N2 notify controller bootstrap.
    //
    // Only wired when `--security --activation-mode file` and
    // `--notify-socket` are all set. The controller debounces per-skill
    // FUSE mutations and sends `skill_ledger.skillfs_notify_change` to the
    // daemon. Notify failure is diagnostic only and never changes the
    // active resolver.
    // N3 protocol event writer bootstrap.
    //
    // Built before the notify controller so it can be injected.
    // When `--activation-events-log` is set but the file cannot be
    // opened, the mount fails at startup.
    let protocol_event_writer: Arc<dyn ProtocolEventWriter> =
        if let Some(ref p) = activation_events_log {
            let writer = JsonlProtocolEventWriter::new(p, 0).map_err(|e| {
                format!(
                    "failed to open --activation-events-log path '{}': {}",
                    p.display(),
                    e
                )
            })?;
            info!(path = %p.display(), "activation protocol event log enabled");
            Arc::new(writer) as Arc<dyn ProtocolEventWriter>
        } else {
            Arc::new(NoopProtocolEventWriter) as Arc<dyn ProtocolEventWriter>
        };

    // A3: Activation reload controller bootstrap.
    //
    // Built before the notify controller so it can be injected.
    // Only constructed when --security --activation-mode file
    // --activation-reload-mode poll and an active resolver exists.
    let reload_controller: Option<Arc<ActivationReloadController>> =
        if reload_mode == ReloadMode::Poll && active_resolver.is_some() {
            let resolver_for_reload = active_resolver
                .clone()
                .expect("active_resolver presence checked above");
            let ctrl = Arc::new(ActivationReloadController::new(
                daemon_root.clone(),
                resolver_for_reload,
                std::time::Duration::from_millis(reload_interval_ms),
                std::time::Duration::from_millis(reload_timeout_ms),
            ));
            info!(
                reload_mode = %reload_mode,
                interval_ms = reload_interval_ms,
                timeout_ms = reload_timeout_ms,
                "activation reload controller enabled"
            );
            Some(ctrl)
        } else {
            None
        };

    let notify_controller: Option<Arc<NotifyController>> = if let Some(ref socket_path) =
        notify_socket
    {
        let timeout = std::time::Duration::from_millis(notify_timeout_ms);
        let client = match notify_auth_key_file.as_deref() {
            Some(key_file) => Arc::new(
                UnixSocketNotifyClient::new_authenticated(socket_path.clone(), timeout, key_file)
                    .map_err(|e| format!("--notify-auth-key-file '{}': {e}", key_file.display()))?,
            ),
            None => Arc::new(UnixSocketNotifyClient::new(socket_path.clone(), timeout)),
        };
        let ctrl = build_notify_controller(
            client,
            &runtime_roots,
            notify_timeout_ms,
            protocol_event_writer.clone(),
            reload_controller.clone(),
        );
        info!(
            socket = %socket_path.display(),
            timeout_ms = notify_timeout_ms,
            reload = reload_mode != ReloadMode::Off,
            "notify: change client enabled (Unix socket)"
        );
        Some(ctrl)
    } else if activation_events_log.is_some() {
        let client = Arc::new(skillfs_fuse::security::NoopNotifyClient);
        let ctrl = build_notify_controller(
            client,
            &runtime_roots,
            DEFAULT_NOTIFY_TIMEOUT_MS,
            protocol_event_writer.clone(),
            reload_controller.clone(),
        );
        info!("notify: protocol event log only (no socket)");
        Some(ctrl)
    } else {
        None
    };

    // Merge trusted-writer-exe: CLI flag overrides config file.
    let trusted_writer_exe = trusted_writer_exe.or_else(|| {
        file_config
            .as_ref()
            .and_then(|c| c.trusted_writer_exe().map(PathBuf::from))
    });

    // Trusted writer gate construction.
    let trusted_writer_config: Option<TrustedWriterConfig> =
        match (&trusted_writer, &trusted_writer_exe) {
            (_, Some(exe_path)) => {
                #[cfg(not(target_os = "linux"))]
                return Err("--trusted-writer-exe requires Linux (/proc/<pid>/exe)".into());

                #[cfg(target_os = "linux")]
                {
                    use skillfs_fuse::security::FileId;
                    use std::os::unix::fs::MetadataExt;

                    let canonical = std::fs::canonicalize(exe_path).map_err(|e| {
                        format!("--trusted-writer-exe '{}': {e}", exe_path.display())
                    })?;
                    let meta = std::fs::metadata(&canonical).map_err(|e| {
                        format!("--trusted-writer-exe '{}': {e}", canonical.display())
                    })?;
                    if !meta.is_file() {
                        return Err(format!(
                            "--trusted-writer-exe '{}': not a regular file",
                            canonical.display()
                        )
                        .into());
                    }
                    let file_id = FileId {
                        dev: meta.dev(),
                        ino: meta.ino(),
                    };
                    let cfg = match &trusted_writer {
                        Some(name) if !name.trim().is_empty() => {
                            TrustedWriterConfig::with_executable_and_compat_name(
                                canonical.clone(),
                                file_id,
                                name.clone(),
                            )
                        }
                        _ => TrustedWriterConfig::with_executable(canonical.clone(), file_id),
                    };
                    info!(
                        trusted_writer_exe = %canonical.display(),
                        "trusted writer enabled (executable identity)"
                    );
                    eprintln!();
                    eprintln!("  --trusted-writer-exe: executable identity pinned (production).");
                    eprintln!("   path = {}", canonical.display());
                    eprintln!("   file_id = ({file_id})");
                    if trusted_writer.is_some() {
                        eprintln!(
                            "   --trusted-writer is also set (compatibility/log context only)."
                        );
                        eprintln!("   Executable identity is the sole authorization basis.");
                    }
                    eprintln!();
                    Some(cfg)
                }
            }
            (Some(name), None) if !name.trim().is_empty() => {
                let cfg = TrustedWriterConfig::with_process_name(name.clone());
                info!(
                    trusted_writer = %name,
                    "trusted writer enabled (compat: TID-to-TGID comm match)"
                );
                eprintln!();
                eprintln!("⚠  --trusted-writer is a deprecated / compatibility gate (comm match).");
                eprintln!("   Process comm can be spoofed (prctl PR_SET_NAME, exec'd basename).");
                eprintln!("   Production: use --trusted-writer-exe <PATH> instead.");
                eprintln!();
                Some(cfg)
            }
            _ => None,
        };

    // ── Trusted peer control socket: resolve the effective endpoint ──
    //
    // CLI/config values were merged and validated earlier (mutual
    // requirement, semantic gates, backing-root gate). Here we only
    // classify the endpoint by priority:
    //   1. --control-socket / [control_socket].path (merged earlier)
    //   2. the default per-user endpoint, when a trusted peer is set but
    //      no explicit path is given.
    let effective_socket_path: Option<PathBuf> = {
        use skillfs_fuse::security::control_socket::{
            EndpointResolution, classify_control_socket_endpoint,
            resolve_default_control_socket_endpoint,
        };
        match classify_control_socket_endpoint(control_socket.as_deref(), has_trusted_peer) {
            EndpointResolution::Explicit(path) => Some(path),
            EndpointResolution::UseDefault => {
                // Trusted peer, no explicit path: bind the default per-user
                // endpoint. Never falls back to /tmp or /var/tmp.
                Some(resolve_default_control_socket_endpoint().map_err(|e| e.to_string())?)
            }
            // The mutual-requirement gate above already rejected an explicit
            // path without a trusted peer.
            EndpointResolution::MissingTrustedPeer(p) => {
                return Err(format!(
                    "--control-socket {} requires a trusted peer authentication mode",
                    p.display()
                )
                .into());
            }
            EndpointResolution::Disabled => None,
        }
    };

    // Build ControlSocketConfig when the control plane is enabled.
    //
    // Hermes layout is compatible: the resolver and activation write methods
    // use full layout-relative skill ids such as `category/skill`.
    let control_socket_server: Option<ControlSocketServer> = match (
        &effective_socket_path,
        &trusted_peer_exe,
        &trusted_peer_key_file,
    ) {
        (Some(socket_path), Some(exe_path), None) => {
            #[cfg(not(target_os = "linux"))]
            return Err("--control-socket requires Linux (SO_PEERCRED, /proc/<pid>/exe)".into());

            #[cfg(target_os = "linux")]
            {
                use skillfs_fuse::security::FileId;
                use std::os::unix::fs::MetadataExt;

                let canonical = std::fs::canonicalize(exe_path)
                    .map_err(|e| format!("--trusted-peer-exe '{}': {e}", exe_path.display()))?;
                let meta = std::fs::metadata(&canonical)
                    .map_err(|e| format!("--trusted-peer-exe '{}': {e}", canonical.display()))?;
                if !meta.is_file() {
                    return Err(format!(
                        "--trusted-peer-exe '{}': not a regular file",
                        canonical.display()
                    )
                    .into());
                }
                let file_id = FileId {
                    dev: meta.dev(),
                    ino: meta.ino(),
                };

                info!(
                    control_socket = %socket_path.display(),
                    trusted_peer_exe = %canonical.display(),
                    trusted_peer_file_id = %file_id,
                    "control socket enabled"
                );

                Some(ControlSocketServer::new(ControlSocketConfig {
                    socket_path: socket_path.clone(),
                    trusted_peer: TrustedPeerConfig {
                        exe_path: canonical,
                        exe_file_id: file_id,
                        uid: trusted_peer_uid,
                        gid: trusted_peer_gid,
                    },
                }))
            }
        }
        (Some(socket_path), None, Some(key_file)) => {
            let server = ControlSocketServer::new_hmac(
                socket_path.clone(),
                key_file,
                trusted_peer_uid,
                trusted_peer_gid,
            )
            .map_err(|e| format!("--trusted-peer-key-file '{}': {e}", key_file.display()))?;
            info!(
                control_socket = %socket_path.display(),
                auth_mode = "hmac-sha256",
                "control socket enabled"
            );
            Some(server)
        }
        _ => None,
    };

    // Mount options
    let options = MountOptions {
        allow_other,
        read_only,
        foreground,
        fuse_options: vec!["noatime".to_string()],
    };

    info!("starting FUSE filesystem (blocking)");

    // mount_canon and in_place were computed earlier, before the A6/B1
    // backing root setup.
    let drift_enabled = audit_sink.is_some();
    if in_place {
        info!("in-place mount detected: FUSE will over-mount the source directory");
        eprintln!();
        eprintln!(
            "⚠  In-place mount: '{}' will be READ-ONLY while SkillFS is running.",
            source.display()
        );
        eprintln!("   To install, update, or remove skills, you MUST unmount first:");
        eprintln!("     fusermount3 -u '{}'", mountpoint.display());
        eprintln!("   or send SIGTERM / press Ctrl+C to stop this process.");
        if security_mode {
            eprintln!();
            eprintln!("   --security-mode is enabled: SkillFS audit and policy now cover");
            eprintln!(
                "   every read/write to '{}' that goes through userspace.",
                source.display()
            );
            if drift_enabled {
                eprintln!(
                    "   --audit-log is also enabled: best-effort source drift observation is"
                );
                eprintln!("   active, surfacing out-of-band create/modify/delete of");
                eprintln!("   <source>/<skill>/SKILL.md and immediate skill directories as");
                eprintln!(
                    "   `source_changed` audit lines (visibility-only, no real-time blocking)."
                );
            }
        }
        eprintln!();
    } else {
        // Non-in-place / "compatibility" mount. SkillFS still serves the
        // virtual skill view at '{mountpoint}/skills/...', but the physical
        // source directory remains directly writable outside FUSE. Be
        // explicit so an operator who relies on .skill-meta protection or
        // the audit log knows where the boundary actually is.
        warn!(
            source = %source.display(),
            mountpoint = %mountpoint.display(),
            "non-in-place mount: SkillFS policy/audit only cover the FUSE mountpoint"
        );
        eprintln!();
        eprintln!("⚠  Non-in-place (compatibility / dev) mount:");
        eprintln!("     source     = '{}'", source.display());
        eprintln!("     mountpoint = '{}'", mountpoint.display());
        eprintln!("   • Direct writes to the source path are NOT routed through SkillFS,");
        eprintln!("     so '.skill-meta' protection and the audit log only cover");
        eprintln!(
            "     operations that go through '{}'.",
            mountpoint.display()
        );
        if drift_enabled {
            eprintln!("   • Source drift observation is enabled (Package W1, best-effort):");
            eprintln!("     out-of-band create/modify/delete of <source>/<skill>/SKILL.md and");
            eprintln!("     immediate skill directories surface as `source_changed` audit lines.");
            eprintln!("     Arbitrary files inside skills, '.skill-meta/**', and nested layouts");
            eprintln!("     are NOT observed; SkillFS does not block in real time.");
        } else {
            eprintln!("   • Source drift observation is OFF (no --audit-log): out-of-band");
            eprintln!("     changes to the source path are not observed at all. Re-run with");
            eprintln!("     --audit-log <PATH> to record SKILL.md / skill-dir drift.");
        }
        eprintln!("   • You can add or remove skill directories at the source at any");
        eprintln!("     time; the change is picked up on the next mount.");
        eprintln!("   For a mount that enforces SkillFS policy on every read/write,");
        eprintln!("   re-run with '--security-mode' and source == mountpoint.");
        eprintln!();
    }

    // Write PID file so the process can be managed without shell job control.
    // e.g. `kill -TERM $(cat /tmp/skillfs.pid)` to unmount cleanly.
    if let Some(ref pid_path) = pid_file {
        let pid = std::process::id();
        std::fs::write(pid_path, format!("{}\n", pid))
            .map_err(|e| format!("failed to write pid file '{}': {}", pid_path.display(), e))?;
        info!(path = %pid_path.display(), pid, "wrote PID file");
    }

    // Capture the mountpoint for signal-triggered cleanup.
    let mountpoint_for_signal = mountpoint.clone();
    let pid_file_for_signal = pid_file.clone();

    // Package W1 source drift observer wiring. Best-effort and visibility-only.
    //
    // When the operator opts in to audit logging via `--audit-log`, attach
    // the existing `skillfs-core` watcher to the same audit sink so
    // out-of-band source-tree changes (especially direct writes to the
    // physical source path in compat mode, and writes through pre-mount
    // file descriptors in security mode) surface as `SourceChanged` JSONL
    // records. Without `--audit-log` this whole block is skipped, so the
    // pre-W1 default behavior is preserved exactly: no watcher is spawned,
    // no drift observation runs, no extra threads are started.
    //
    // Failures are non-fatal. Drift observation is a visibility aid: a
    // failed watcher startup must not abort the FUSE mount that an
    // operator already asked for. We log a warning and continue with the
    // sink-only audit pipeline that S2.1 delivered.
    let drift_handle = if let Some(ref sink) = audit_sink {
        let drift_source = runtime_roots.daemon_root().to_path_buf();
        let observer = Arc::new(SourceDriftObserver::new(drift_source.clone(), sink.clone()));
        match spawn_drift_watcher(drift_source.clone(), observer, DRIFT_DEBOUNCE_MS).await {
            Ok(handle) => {
                info!(
                    source = %drift_source.display(),
                    debounce_ms = DRIFT_DEBOUNCE_MS,
                    "source drift observation enabled"
                );
                Some(handle)
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "failed to start source drift watcher; continuing without drift observation"
                );
                None
            }
        }
    } else {
        None
    };

    // A5: Activation state watcher. Background convergence loop that
    // periodically checks activation freshness and reloads when the
    // daemon writes new activation. Independent of FUSE event loop.
    let activation_watcher: Option<Arc<ActivationWatcher>> =
        if reload_mode == ReloadMode::Poll && reload_controller.is_some() {
            let reload_for_watcher = reload_controller
                .clone()
                .expect("reload_controller presence checked above");
            let watcher = Arc::new(ActivationWatcher::new(
                reload_for_watcher,
                protocol_event_writer.clone(),
                std::time::Duration::from_millis(watcher_interval_ms),
            ));
            info!(
                watcher_interval_ms = watcher_interval_ms,
                "activation watcher enabled (continuous convergence)"
            );
            Some(watcher)
        } else {
            None
        };

    // A5: inject watcher registrar into notify controller so new skills
    // observed through FUSE mutations are automatically tracked.
    if let (Some(watcher), Some(ctrl)) = (&activation_watcher, &notify_controller) {
        ctrl.set_watcher_registrar(watcher.clone());
    }

    // A4: capture reconcile inputs before notify_controller is moved into
    // MountConfig. Reconcile fires once after mount startup when
    // --security --activation-mode file and a notify controller exists.
    let reconcile_notify = notify_controller.clone();
    let reconcile_skill_names: Option<Vec<String>> =
        if activation_mode == ActivationMode::File && notify_controller.is_some() {
            let names: Vec<String> = if skill_layout == Some(skillfs_fuse::SkillLayout::Hermes) {
                skillfs_fuse::security::enumerate_hermes_skill_ids(&daemon_root)
            } else {
                shared_store
                    .read()
                    .list()
                    .iter()
                    .filter(|n| **n != "skill-discover")
                    .map(|s| s.to_string())
                    .collect()
            };
            Some(names)
        } else {
            None
        };

    // I2: build staging matcher and controller from [install] config.
    let staging_matcher: Option<Arc<StagingMatcher>> = file_config
        .as_ref()
        .and_then(|c| c.staging_config())
        .map(|cfg| {
            info!(
                patterns = cfg.patterns.len(),
                "staging: installer staging compatibility enabled"
            );
            Arc::new(StagingMatcher::new(cfg))
        });
    let staging_controller: Option<Arc<InstallerStagingController>> =
        match (&staging_matcher, &notify_controller) {
            (Some(matcher), Some(notify_ctrl)) => Some(InstallerStagingController::new(
                matcher.clone(),
                notify_ctrl.clone(),
            )),
            _ => None,
        };

    let quiet_timeout_controller = match &notify_controller {
        Some(notify_ctrl) => file_config
            .as_ref()
            .and_then(|c| c.quiet_timeout_ms())
            .map(|ms| {
                info!(
                    quiet_timeout_ms = ms,
                    "install: quiet-timeout mutation notify enabled"
                );
                skillfs_fuse::security::QuietTimeoutController::new(
                    notify_ctrl.clone(),
                    std::time::Duration::from_millis(ms),
                )
            }),
        None => {
            if file_config
                .as_ref()
                .and_then(|c| c.quiet_timeout_ms())
                .is_some()
            {
                warn!(
                    "install: quiet_timeout_ms configured but no notify controller; \
                     quiet timeout disabled"
                );
            }
            None
        }
    };

    // I4: Build post-publish grace controller from [install] config.
    // Must be built before PendingInstallController so we can inject it.
    let post_publish_controller = match (
        file_config.as_ref().and_then(|c| c.post_publish_grace_ms()),
        file_config
            .as_ref()
            .and_then(|c| c.post_publish_write_patterns()),
    ) {
        (Some(ms), Some(patterns)) => {
            let parsed = skillfs_fuse::security::validate_post_publish_patterns(patterns)
                .map_err(|e| format!("invalid install.post_publish_write_patterns: {e}"))?;
            info!(
                post_publish_grace_ms = ms,
                patterns = parsed.len(),
                "install: post-publish grace window enabled"
            );
            Some(skillfs_fuse::security::PostPublishGraceController::new(
                std::time::Duration::from_millis(ms),
                parsed,
            ))
        }
        _ => None,
    };

    let pending_install_controller = match (&notify_controller, &active_resolver) {
        (Some(notify_ctrl), Some(_)) => file_config
            .as_ref()
            .and_then(|c| c.quiet_timeout_ms())
            .map(|ms| {
                info!(
                    pending_timeout_ms = ms,
                    "install: direct final-skill pending install enabled"
                );
                skillfs_fuse::security::PendingInstallController::new_with_post_publish(
                    notify_ctrl.clone(),
                    std::time::Duration::from_millis(ms),
                    daemon_root.clone(),
                    post_publish_controller.clone(),
                )
            }),
        _ => None,
    };

    // Start control socket server before the FUSE mount.
    let control_socket_handle = if let Some(server) = control_socket_server {
        let ctx = ControlSocketContext {
            // Canonical root: the user-visible Skill root the ledger
            // addresses. Live root (source_root): the physical backing
            // tree that stays accessible under the FUSE over-mount.
            canonical_root: runtime_roots.notify_canonical_root().to_path_buf(),
            source_root: daemon_root.clone(),
            // Layout drives the resolver's Flat / Hermes Skill boundary.
            layout: skill_layout.unwrap_or_default(),
            resolver: active_resolver.clone(),
            protocol_event_writer: Some(protocol_event_writer.clone()),
        };
        let handle = server
            .with_context(ctx)
            .start()
            .map_err(|e| format!("failed to start control socket server: {e}"))?;
        info!(
            socket = %handle.socket_path().display(),
            "control socket server started"
        );
        Some(handle)
    } else {
        None
    };

    // mount_configured() blocks until the FUSE session exits (Ctrl+C or
    // SIGTERM). We wrap it in spawn_blocking and race against OS signals
    // so that SIGTERM triggers the same clean unmount path as Ctrl+C.
    //
    // A6/B1: `backing_root` stays in this scope and is dropped when
    // cmd_mount returns. The Drop impl calls cleanup(), which unmounts
    // the bind mount and removes the temp dir. The bind mount is
    // independent of the FUSE mount, so cleanup order does not matter.

    // --- Session stats: create collector before mount starts ---
    let session_stats = Arc::new(SkillfsSessionStats::new());
    // Captured for the runtime `view_pruned` metric event (emitted below).
    let runtime_pruned_skill_count: u64;
    let runtime_token_saved_estimate: u64;
    {
        // Load ViewsConfig once; derive all metrics from a single canonical set.
        let store_guard = shared_store.read();
        let total_skills = store_guard.len() as u64;
        let views_config = ViewsConfig::load(runtime_roots.physical_source_root());

        // Build the canonical "real existing default-view skills" set.
        // Deduplicates and filters out stale/typo names not in the store.
        let default_served: std::collections::HashSet<String> = if let Some(ref cfg) = views_config
        {
            cfg.effective_default_skills(&store_guard)
                .into_iter()
                .collect()
        } else {
            // No views config => all skills are default.
            store_guard.list().into_iter().map(str::to_string).collect()
        };

        let default_exposed_count = default_served.len() as u64;
        session_stats.set_skill_counts(total_skills, default_exposed_count);
        runtime_pruned_skill_count = total_skills.saturating_sub(default_exposed_count);

        // prompt_token_saved_estimate: body chars of pruned skills / 4.
        // Pruned = in store but NOT in default_served.
        let has_views_config = views_config.is_some();
        let token_estimate = if has_views_config {
            let mut pruned_chars: u64 = 0;
            for name in store_guard.list() {
                if !default_served.contains(name) {
                    if let Some(entry) = store_guard.get(name) {
                        pruned_chars += entry.body.len() as u64;
                    }
                }
            }
            pruned_chars / 4
        } else {
            // No views config => nothing pruned => 0.
            0
        };
        session_stats.set_prompt_token_saved_estimate(token_estimate);
        runtime_token_saved_estimate = token_estimate;

        // V1 skill_hit_times: default-view served skills (deduplicated).
        for name in &default_served {
            session_stats.record_skill_hit(name);
        }
    }

    // Feed initial decision outcomes from activation bootstrap.
    // When active_resolver exists, count from resolver snapshot.
    // When absent (non-security / normal mount), all default-served skills
    // are effectively "allowed" (live source passthrough).
    if let Some(ref resolver) = active_resolver {
        for (_name, target) in resolver.snapshot() {
            match &target {
                skillfs_fuse::security::ActiveTarget::Current { .. } => {
                    session_stats.record_decision(RuntimeDecisionOutcome::Allow);
                }
                skillfs_fuse::security::ActiveTarget::Snapshot { .. } => {
                    session_stats.record_decision(RuntimeDecisionOutcome::Fallback);
                }
                skillfs_fuse::security::ActiveTarget::Hidden { .. } => {
                    session_stats.record_decision(RuntimeDecisionOutcome::Deny);
                }
            }
        }
    } else {
        // No security resolver => all served skills are implicitly allowed.
        // Use pruned_skill_count helper: default_exposed = total - pruned.
        let default_exposed = shared_store.read().len() as u64 - session_stats.pruned_skill_count();
        for _ in 0..default_exposed {
            session_stats.record_decision(RuntimeDecisionOutcome::Allow);
        }
    }

    // Generate session ID from PID + nanosecond timestamp (collision-resistant).
    let session_id_for_flush = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    // Resolve agent_name: env var ANOLISA_AGENT_NAME, default "agent".
    let agent_name_for_flush =
        std::env::var("ANOLISA_AGENT_NAME").unwrap_or_else(|_| "agent".to_string());
    let stats_for_flush = session_stats.clone();

    // --- Runtime metrics: real-time delta events for the SLS ops log ---
    // Best-effort: writes are skipped when the deployment-owned file is absent
    // and never affect mount behavior or exit status.
    let runtime_metrics = Arc::new(RuntimeMetricsSink::new(
        RuntimeMetricsWriter::new(sls_ops::resolve_ops_log_path()),
        session_id_for_flush.clone(),
        agent_name_for_flush.clone(),
    ));
    // `view_pruned`: emitted once at startup after views are computed.
    runtime_metrics.emit_view_pruned(runtime_pruned_skill_count, runtime_token_saved_estimate);

    // Mark mount ready immediately before spawning — the FUSE session
    // start is the closest signal we have.
    session_stats.mark_mount_ready();

    // Clone for the FUSE runtime; the original stays in this scope for the
    // lifecycle events (mount_start / heartbeat / mount_end / mount_error).
    let mount_runtime_metrics = runtime_metrics.clone();
    let mount_task = tokio::task::spawn_blocking(move || {
        mount_configured(
            &mountpoint,
            runtime_roots.physical_source_root(),
            shared_store,
            options,
            in_place,
            MountConfig {
                event_sink: audit_sink,
                policy: None,
                active_resolver,
                refresh_controller,
                notify_controller,
                trusted_writer: trusted_writer_config,
                staging_matcher,
                staging_controller,
                quiet_timeout_controller,
                pending_install_controller,
                post_publish_controller,
                runtime_metrics: Some(mount_runtime_metrics),
                skill_layout,
                os_adapter: os_adapter_stage,
                directive_enabled,
                skill_discover_root,
            },
        )
    });

    // `mount_start`: the FUSE task spawned successfully.
    runtime_metrics.emit_mount_start();

    // Emit a periodic `mount_heartbeat` (duration delta) while the mount is
    // alive. Aborted on every exit path below.
    let heartbeat_metrics = runtime_metrics.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.tick().await; // consume the immediate first tick
        loop {
            ticker.tick().await;
            heartbeat_metrics.emit_heartbeat();
        }
    });

    // A5: start activation watcher after mount is spawned.
    if let Some(ref watcher) = activation_watcher {
        if let Some(ref names) = reconcile_skill_names {
            watcher.register_skills(names);
        }
        watcher.start();
    }

    // A4: queue startup reconcile after mount is spawned. Delivery is owned
    // by the notify worker, which retries transient failures (daemon socket
    // not created yet, connection refused) with exponential backoff, so a
    // SkillFS-before-daemon start order still converges. Enqueueing is
    // non-blocking and cannot delay startup.
    // A5: after reconcile, schedule an immediate watcher check so
    // daemon-written activation is picked up without waiting for the
    // periodic interval.
    if let (Some(ctrl), Some(names)) = (reconcile_notify, &reconcile_skill_names) {
        let names_owned = names.clone();
        let watcher_for_reconcile = activation_watcher.clone();
        ctrl.enqueue_startup_reconcile(&names_owned);
        if let Some(ref w) = watcher_for_reconcile {
            w.schedule_immediate_check(names_owned);
        }
    }

    /// Flush session stats summary to the session metrics log (at most once).
    /// Best-effort: failure only warns, never changes exit status.
    fn flush_session_stats(stats: &SkillfsSessionStats, session_id: &str, agent_name: &str) {
        let Some(summary) = stats.try_build_summary_once(session_id, agent_name) else {
            // Already flushed by another exit path — skip.
            return;
        };
        let writer = SessionStatsWriter::default_path();
        match writer.write_summary_with_outcome(&summary) {
            Ok(SummaryWriteOutcome::Written) => {
                info!(
                    path = %writer.path().display(),
                    session_id = %session_id,
                    mount_duration_ms = summary.mount_duration_ms,
                    skill_hit_times = summary.skill_hit_times,
                    "session stats: summary flushed to session metrics log"
                );
            }
            Ok(SummaryWriteOutcome::SkippedDisabled) => {
                // Telemetry disabled by sentinel: nothing was written, so do
                // not claim a flush. Normal state, not an error.
                debug!(
                    path = %writer.path().display(),
                    session_id = %session_id,
                    "session stats: telemetry disabled, summary not written"
                );
            }
            Err(e) => {
                warn!(
                    error = %e,
                    path = %writer.path().display(),
                    "session stats: failed to flush summary (non-fatal)"
                );
            }
        }
    }

    /// Trigger a clean FUSE unmount by calling fusermount3 -u.
    /// This causes fuser::mount2 event loop to exit, which unblocks the
    /// spawn_blocking thread and allows the process to exit cleanly.
    fn trigger_unmount(mountpoint: &std::path::Path) {
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mountpoint.to_string_lossy()])
            .output();
    }

    let result = tokio::select! {
        res = mount_task => {
            match res {
                Ok(inner) => inner,
                Err(e) => Err(FuseErr::MountFailed(e.to_string())),
            }
        }
        _ = signal::ctrl_c() => {
            info!("received Ctrl+C, unmounting");
            trigger_unmount(&mountpoint_for_signal);
            // Clean exit: stop heartbeats and emit the final runtime metric.
            heartbeat_handle.abort();
            runtime_metrics.emit_mount_end();
            // Flush session stats before exit.
            flush_session_stats(&stats_for_flush, &session_id_for_flush, &agent_name_for_flush);
            if let Some(h) = drift_handle {
                h.shutdown().await;
            }
            if let Some(h) = control_socket_handle {
                h.shutdown();
            }
            cleanup_pid_file(&pid_file_for_signal);
            return Ok(());
        }
        _ = async {
            let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");
            term.recv().await
        } => {
            info!("received SIGTERM, unmounting");
            trigger_unmount(&mountpoint_for_signal);
            // Clean exit: stop heartbeats and emit the final runtime metric.
            heartbeat_handle.abort();
            runtime_metrics.emit_mount_end();
            // Flush session stats before exit.
            flush_session_stats(&stats_for_flush, &session_id_for_flush, &agent_name_for_flush);
            if let Some(h) = drift_handle {
                h.shutdown().await;
            }
            if let Some(h) = control_socket_handle {
                h.shutdown();
            }
            cleanup_pid_file(&pid_file_for_signal);
            return Ok(());
        }
    };

    // Mount exited on its own (FUSE event loop returned). Stop heartbeats.
    heartbeat_handle.abort();

    // Make sure the drift watcher does not outlive the mount it was paired
    // with: shut it down explicitly so the underlying notify watcher and the
    // drift adapter task are torn down deterministically before this function
    // returns.
    if let Some(h) = drift_handle {
        h.shutdown().await;
    }
    // Shut down control socket server deterministically.
    if let Some(h) = control_socket_handle {
        h.shutdown();
    }

    cleanup_pid_file(&pid_file);

    match result {
        Ok(()) => {
            // Clean exit: emit the final runtime metric delta.
            runtime_metrics.emit_mount_end();
            // Only flush session stats on successful mount exit.
            // Failed mounts must not write mount_times=1.
            flush_session_stats(
                &stats_for_flush,
                &session_id_for_flush,
                &agent_name_for_flush,
            );
            info!("filesystem unmounted successfully");
            Ok(())
        }
        Err(e) => {
            // Mount failed — emit a runtime error metric, and do NOT write a
            // success summary.
            let reason = format!("Mount failed: {}", e);
            runtime_metrics.emit_mount_error(&reason);
            info!("mount failed; skipping session stats flush");
            Err(reason.into())
        }
    }
}

// ---------------------------------------------------------------------------
// Classify Command
// ---------------------------------------------------------------------------

async fn cmd_classify(
    source: PathBuf,
    primary_count: usize,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(source = %source.display(), "classifying skills");

    if !source.exists() {
        return Err(format!("Source directory does not exist: {}", source.display()).into());
    }
    if !source.is_dir() {
        return Err(format!("Source is not a directory: {}", source.display()).into());
    }

    let mut store = SkillStore::new();
    let config = ParseConfig::default();
    let _errors = store.load_from_directory(&source, &config);

    let mut all_names: Vec<String> = store.list().iter().map(|s| s.to_string()).collect();
    all_names.sort();

    if all_names.is_empty() {
        println!("No skills found in {}", source.display());
        return Ok(());
    }

    // If a views config already exists, report its status instead of overwriting.
    if let Some(existing) = ViewsConfig::load(&source) {
        println!("skillfs-views.toml already exists in {}", source.display());
        println!();
        for view in &existing.views {
            let marker = if view.default { " [default]" } else { "" };
            println!("View: {}{}", view.name, marker);
            if !view.description.is_empty() {
                println!("  Description: {}", view.description);
            }
            println!("  Skills ({}):", view.skills.len());
            for s in &view.skills {
                println!("    - {}", s);
            }
            println!();
        }
        let assigned = existing.all_assigned_skills();
        let unassigned: Vec<&String> = all_names
            .iter()
            .filter(|n| !assigned.contains(*n))
            .collect();
        if !unassigned.is_empty() {
            println!("Unassigned skills (will be added to default view on next mount):");
            for s in &unassigned {
                println!("  - {}", s);
            }
        }
        return Ok(());
    }

    // Generate a fresh config: first N skills in "major" (default), rest in "other".
    let n = primary_count.min(all_names.len());
    let primary: Vec<String> = all_names[..n].to_vec();
    let secondary: Vec<String> = all_names[n..].to_vec();

    let cfg = ViewsConfig {
        views: vec![
            skillfs_core::views::ViewConfig {
                name: "major".to_string(),
                default: true,
                description: "Core skills shown at mount time".to_string(),
                skills: primary.clone(),
            },
            skillfs_core::views::ViewConfig {
                name: "other".to_string(),
                default: false,
                description: "Additional skills accessible via skill-discover".to_string(),
                skills: secondary.clone(),
            },
        ],
    };

    if dry_run {
        println!(
            "[dry-run] Would write skillfs-views.toml to {}",
            source.display()
        );
        println!();
        println!("Primary view 'major' ({} skills):", primary.len());
        for s in &primary {
            println!("  - {}", s);
        }
        println!();
        println!("Secondary view 'other' ({} skills):", secondary.len());
        for s in &secondary {
            println!("  - {}", s);
        }
    } else {
        cfg.save(&source)?;
        println!("Written skillfs-views.toml to {}", source.display());
        println!();
        println!("Primary view 'major' ({} skills):", primary.len());
        for s in &primary {
            println!("  - {}", s);
        }
        println!();
        println!("Secondary view 'other' ({} skills):", secondary.len());
        for s in &secondary {
            println!("  - {}", s);
        }
        println!();
        println!("Edit skillfs-views.toml to move skills between views as needed.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Validate Command
// ---------------------------------------------------------------------------

/// Validate skills in `source`.
///
/// Returns `(result, validation_failed)`. `validation_failed` is `true` when
/// one or more skills failed to load or parse; the caller records an SLS ops
/// record and then exits non-zero. Directory-level problems are returned as
/// `Err` in the result.
async fn cmd_validate(
    source: PathBuf,
    format: OutputFormat,
) -> (Result<(), Box<dyn std::error::Error>>, bool) {
    info!(source = %source.display(), "validating skills");

    if !source.exists() {
        return (
            Err(format!("Source directory does not exist: {}", source.display()).into()),
            false,
        );
    }
    if !source.is_dir() {
        return (
            Err(format!("Source is not a directory: {}", source.display()).into()),
            false,
        );
    }

    let mut store = SkillStore::new();
    let config = ParseConfig::default();
    let load_errors = store.load_from_directory(&source, &config);

    let mut success: usize = 0;
    let mut degraded: usize = 0;
    let mut parse_error_count: usize = 0;

    let names = store.list();
    for name in &names {
        if let Some(entry) = store.get(name) {
            match &entry.parse_status {
                skillfs_core::ParseStatus::Ok => success += 1,
                skillfs_core::ParseStatus::Degraded(_) => degraded += 1,
                skillfs_core::ParseStatus::Error(_) => parse_error_count += 1,
            }
        }
    }

    let failed = parse_error_count + load_errors.len();
    let total = success + degraded + failed;

    match format {
        OutputFormat::Text => {
            println!("Validated {} skills from {}", total, source.display());

            if failed == 0 && degraded == 0 {
                println!("✓ All skills loaded successfully");
            } else {
                if failed > 0 {
                    println!("✗ {} skill(s) failed:", failed);
                    for err in &load_errors {
                        println!("  - {}: {}", err.path.display(), err.error);
                    }
                    for name in &names {
                        if let Some(entry) = store.get(name) {
                            if entry.parse_status.is_error() {
                                println!("  - {}: {}", name, entry.parse_status.message());
                            }
                        }
                    }
                }
                if degraded > 0 {
                    println!("⚠ {} skill(s) degraded:", degraded);
                    for name in &names {
                        if let Some(entry) = store.get(name) {
                            if entry.parse_status.is_degraded() {
                                println!("  - {}: {}", name, entry.parse_status.message());
                            }
                        }
                    }
                }
            }

            if !store.is_empty() {
                println!("\nSkills:");
                for name in &names {
                    if let Some(entry) = store.get(name) {
                        let status = match &entry.parse_status {
                            skillfs_core::ParseStatus::Ok => "✓",
                            skillfs_core::ParseStatus::Degraded(_) => "⚠",
                            skillfs_core::ParseStatus::Error(_) => "✗",
                        };
                        println!(
                            "  {} {} - {} ({})",
                            status,
                            name,
                            entry
                                .metadata
                                .description
                                .chars()
                                .take(50)
                                .collect::<String>(),
                            if entry.metadata.enabled {
                                "enabled"
                            } else {
                                "disabled"
                            }
                        );
                    }
                }
            }
        }
        OutputFormat::Json => {
            let mut error_entries: Vec<serde_json::Value> = load_errors
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "path": e.path.to_string_lossy().to_string(),
                        "status": "load_error",
                        "message": e.error
                    })
                })
                .collect();
            for name in &names {
                if let Some(entry) = store.get(name) {
                    if entry.parse_status.is_error() {
                        error_entries.push(serde_json::json!({
                            "name": name,
                            "path": entry.source_path.to_string_lossy().to_string(),
                            "status": "error",
                            "message": entry.parse_status.message()
                        }));
                    }
                }
            }

            let mut warning_entries: Vec<serde_json::Value> = Vec::new();
            for name in &names {
                if let Some(entry) = store.get(name) {
                    if entry.parse_status.is_degraded() {
                        warning_entries.push(serde_json::json!({
                            "name": name,
                            "path": entry.source_path.to_string_lossy().to_string(),
                            "status": "degraded",
                            "message": entry.parse_status.message()
                        }));
                    }
                }
            }

            let result = serde_json::json!({
                "total": total,
                "success": success,
                "degraded": degraded,
                "failed": failed,
                "errors": error_entries,
                "warnings": warning_entries,
                "skills": names.iter().map(|name| {
                    if let Some(entry) = store.get(name) {
                        serde_json::json!({
                            "name": name,
                            "description": entry.metadata.description,
                            "enabled": entry.metadata.enabled,
                            "status": entry.parse_status.status_str()
                        })
                    } else {
                        serde_json::json!({})
                    }
                }).collect::<Vec<_>>()
            });
            match serde_json::to_string_pretty(&result) {
                Ok(s) => println!("{}", s),
                Err(e) => return (Err(e.into()), failed > 0),
            }
        }
    }

    (Ok(()), failed > 0)
}

// ---------------------------------------------------------------------------
// List Command
// ---------------------------------------------------------------------------

async fn cmd_list(source: PathBuf, enabled_only: bool) -> Result<(), Box<dyn std::error::Error>> {
    info!(source = %source.display(), "listing skills");

    // Validate source directory
    if !source.exists() {
        return Err(format!("Source directory does not exist: {}", source.display()).into());
    }
    if !source.is_dir() {
        return Err(format!("Source is not a directory: {}", source.display()).into());
    }

    // Load skills
    let mut store = SkillStore::new();
    let config = ParseConfig::default();
    let _errors = store.load_from_directory(&source, &config);

    let names = store.list();

    if names.is_empty() {
        println!("No skills found in {}", source.display());
        return Ok(());
    }

    println!("Skills in {}:", source.display());
    println!();

    for name in names {
        if let Some(entry) = store.get(name) {
            if enabled_only && !entry.metadata.enabled {
                continue;
            }

            let status_icon = match &entry.parse_status {
                skillfs_core::ParseStatus::Ok => "✓",
                skillfs_core::ParseStatus::Degraded(_) => "⚠",
                skillfs_core::ParseStatus::Error(_) => "✗",
            };

            println!("{} {}", status_icon, name);
            println!("  Description: {}", entry.metadata.description);
            println!("  Version: {}", entry.metadata.version);
            println!(
                "  Tags: {}",
                if entry.metadata.tags.is_empty() {
                    "(none)".to_string()
                } else {
                    entry.metadata.tags.join(", ")
                }
            );
            println!(
                "  Status: {} | {}",
                format!("{:?}", entry.parse_status).to_lowercase(),
                if entry.metadata.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            println!();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use skillfs_fuse::security::{
        ActiveSkillResolver, InMemoryProtocolEventWriter, MutationKind, NOTIFY_METHOD,
        NotifyChangeEvent, NotifyClient, NotifyError,
    };

    use super::*;

    #[derive(Debug, Clone)]
    struct CapturedNotify {
        schema_version: u64,
        method: &'static str,
        canonical_skill_dir: String,
        skill_id: String,
        event_kind: String,
        paths: Vec<String>,
    }

    struct ActivationWritingNotifyClient {
        activation_path: PathBuf,
        events: Mutex<Vec<CapturedNotify>>,
    }

    impl ActivationWritingNotifyClient {
        fn new(activation_path: PathBuf) -> Self {
            Self {
                activation_path,
                events: Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<CapturedNotify> {
            self.events.lock().clone()
        }
    }

    impl NotifyClient for ActivationWritingNotifyClient {
        fn send(&self, event: &NotifyChangeEvent) -> Result<(), NotifyError> {
            self.events.lock().push(CapturedNotify {
                schema_version: event.params.schema_version,
                method: event.method,
                canonical_skill_dir: event.params.canonical_skill_dir.clone(),
                skill_id: event.params.skill_id.clone(),
                event_kind: event.params.event_kind.clone(),
                paths: event.params.paths.clone(),
            });
            let before = std::fs::metadata(&self.activation_path)
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            for _ in 0..100 {
                std::thread::sleep(Duration::from_millis(15));
                std::fs::write(
                    &self.activation_path,
                    r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
                )
                .expect("write activation during notify");
                let after = std::fs::metadata(&self.activation_path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok());
                if before.map_or(after.is_some(), |before| {
                    after.is_some_and(|after| after > before)
                }) {
                    return Ok(());
                }
            }
            panic!("activation mtime did not advance");
        }
    }

    fn seed_skill(root: &Path, skill_id: &str) -> PathBuf {
        let skill_dir = root.join(skill_id);
        let snapshot = skill_dir.join(".skill-meta/versions/v000001.snapshot");
        std::fs::create_dir_all(&snapshot).expect("create snapshot");
        std::fs::write(
            snapshot.join("SKILL.md"),
            format!("---\nname: {skill_id}\ndescription: fixture\n---\n"),
        )
        .expect("write snapshot skill");
        skill_dir.join(".skill-meta/activation.json")
    }

    #[test]
    fn mount_runtime_roots_preserve_notify_identity_and_select_live_root() {
        let source_roots = SourceRoots {
            canonical_identity_root: PathBuf::from("/srv/skills"),
            physical_source_root: PathBuf::from("/srv/skills"),
        };
        let backing_root = Path::new("/run/skillfs-ledger/skills");

        let direct = MountRuntimeRoots::from_source(&source_roots, None);
        assert_eq!(
            direct.notify_canonical_root(),
            source_roots.canonical_identity_root()
        );
        assert_eq!(direct.daemon_root(), source_roots.physical_source_root());

        let backed = MountRuntimeRoots::from_source(&source_roots, Some(backing_root));
        assert_eq!(
            backed.notify_canonical_root(),
            source_roots.canonical_identity_root()
        );
        assert_eq!(backed.daemon_root(), backing_root);
        assert_ne!(backed.notify_canonical_root(), backed.daemon_root());
    }

    #[test]
    fn source_roots_preserve_symlink_identity_and_resolve_physical_source() {
        let physical_root = tempfile::tempdir().unwrap();
        let identity_parent = tempfile::tempdir().unwrap();
        let identity_root = identity_parent.path().join("skills-link");
        std::os::unix::fs::symlink(physical_root.path(), &identity_root).unwrap();

        let source_roots = SourceRoots::resolve(&identity_root).unwrap();
        assert_eq!(source_roots.canonical_identity_root(), identity_root);
        assert_eq!(
            source_roots.physical_source_root(),
            physical_root.path().canonicalize().unwrap()
        );

        let runtime_roots = MountRuntimeRoots::from_source(&source_roots, None);
        assert_eq!(runtime_roots.notify_canonical_root(), identity_root);
        assert_eq!(
            runtime_roots.daemon_root(),
            physical_root.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn notify_controller_wiring_splits_canonical_and_event_roots() {
        let physical_source_root = tempfile::tempdir().unwrap();
        let identity_parent = tempfile::tempdir().unwrap();
        let canonical_identity_root = identity_parent.path().join("skills-link");
        std::os::unix::fs::symlink(physical_source_root.path(), &canonical_identity_root).unwrap();
        let daemon_root = tempfile::tempdir().unwrap();
        let skill_id = "category/alpha";
        let activation_path = seed_skill(daemon_root.path(), skill_id);
        seed_skill(physical_source_root.path(), skill_id);

        let client = Arc::new(ActivationWritingNotifyClient::new(activation_path));
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let resolver = Arc::new(ActiveSkillResolver::new(
            physical_source_root.path().to_path_buf(),
        ));
        let reload_controller = Arc::new(ActivationReloadController::new(
            daemon_root.path().to_path_buf(),
            resolver,
            Duration::from_millis(1),
            Duration::from_millis(250),
        ));
        let source_roots = SourceRoots::resolve(&canonical_identity_root).unwrap();
        let runtime_roots = MountRuntimeRoots::from_source(&source_roots, Some(daemon_root.path()));

        assert_eq!(
            runtime_roots.notify_canonical_root(),
            canonical_identity_root
        );
        assert_eq!(
            runtime_roots.physical_source_root(),
            physical_source_root.path().canonicalize().unwrap()
        );
        assert_eq!(runtime_roots.daemon_root(), daemon_root.path());

        let ctrl = build_notify_controller(
            client.clone(),
            &runtime_roots,
            DEFAULT_NOTIFY_TIMEOUT_MS,
            writer.clone(),
            Some(reload_controller),
        );

        let count = ctrl.emit_startup_reconcile(&[skill_id.to_string()]);
        assert_eq!(count, 1);

        ctrl.observe(skill_id, Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.flush_for_testing();

        let notify_events = client.events();
        assert_eq!(notify_events.len(), 2);
        for event in &notify_events {
            assert_eq!(event.method, NOTIFY_METHOD);
            assert_eq!(event.schema_version, 2);
            assert_eq!(event.skill_id, skill_id);
            assert!(
                event
                    .canonical_skill_dir
                    .starts_with(canonical_identity_root.to_string_lossy().as_ref())
            );
            assert!(
                !event
                    .canonical_skill_dir
                    .starts_with(physical_source_root.path().to_string_lossy().as_ref()),
                "notify canonicalSkillDir must preserve symlink identity"
            );
            assert!(
                !event
                    .canonical_skill_dir
                    .starts_with(daemon_root.path().to_string_lossy().as_ref()),
                "notify canonicalSkillDir must not expose daemon root"
            );
        }
        assert_eq!(notify_events[0].event_kind, "reconcile");
        assert!(notify_events[0].paths.is_empty());
        assert_eq!(notify_events[1].event_kind, "write");
        assert_eq!(notify_events[1].paths, vec!["SKILL.md"]);

        let protocol_events = writer.events();
        assert_eq!(protocol_events.len(), 3);
        assert_eq!(protocol_events[0].schema_version, 1);
        assert_eq!(protocol_events[0].event_kind, "reconcile");
        assert_eq!(protocol_events[1].event_kind, "write");
        assert_eq!(
            protocol_events[2].reload_outcome.as_deref(),
            Some("activation_updated")
        );
        for event in &protocol_events {
            assert_eq!(event.skill_name, skill_id);
            assert!(
                event
                    .skill_dir
                    .starts_with(daemon_root.path().to_string_lossy().as_ref()),
                "protocol event skillDir must use daemon root"
            );
            assert!(
                !event
                    .skill_dir
                    .starts_with(canonical_identity_root.to_string_lossy().as_ref()),
                "protocol event skillDir must not use canonical root"
            );
        }

        ctrl.shutdown();
    }
}
