use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};
use clap::builder::{StringValueParser, TypedValueParser};
use clap::{ArgGroup, Args, Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use ws_ckpt_common::{
    decode_payload, default_auto_cleanup_keep, encode_frame, load_config_file, save_config_file,
    ChangeType, CleanupRetention, DaemonConfig, ErrorCode, GlobalConfigJson, PolicyFieldOp,
    RecoveryPreview, Request, Response, SnapshotListItem, WorkspacePolicyJson,
    ADVISORY_SNAPSHOT_LIMIT, CONFIG_FILE_PATH, DEFAULT_AUTO_CLEANUP,
    DEFAULT_AUTO_CLEANUP_INTERVAL_SECS, DEFAULT_HEALTH_CHECK_INTERVAL_SECS,
    DEFAULT_IMG_MAX_PERCENT, DEFAULT_IMG_SIZE_GB, DEFAULT_MOUNT_PATH, DEFAULT_SOCKET_PATH,
    GLOBAL_CONFIG_JSON_SCHEMA, MAX_FRAME_SIZE, MAX_LIST_PAGE_ITEMS, OVERVIEW_JSON_SCHEMA,
};

use std::cell::RefCell;

/// Backend-usage advisory threshold (percent); CLI-side since daemon returns raw bytes.
const ADVISORY_FS_USAGE_PCT: f64 = 90.0;
const ADVISORY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(30);
const DEFAULT_LIST_PAGE_LIMIT: u32 = 1000;

// JSON output buffer: handlers stage their payload here so main() can wrap
// it with `elapsed_secs` and emit a single JSON object on stdout.
thread_local! {
    static JSON_OUTPUT: RefCell<Option<serde_json::Value>> = const { RefCell::new(None) };
}

fn emit_json(value: serde_json::Value) {
    JSON_OUTPUT.with(|buf| *buf.borrow_mut() = Some(value));
}

fn take_json_output() -> Option<serde_json::Value> {
    JSON_OUTPUT.with(|buf| buf.borrow_mut().take())
}

// Parse CLI value for `--auto-cleanup-keep`: integer -> Count mode, duration
// string (e.g. "30d", units s/m/h/d/w) -> Age mode. Mirrors TOML semantics in
// `CleanupRetention::deserialize`.
fn parse_cleanup_retention(s: &str) -> Result<CleanupRetention, String> {
    if let Ok(n) = s.parse::<u32>() {
        return Ok(CleanupRetention::Count(n));
    }
    CleanupRetention::age(s).map_err(|e| format!("invalid value '{}': {}", s, e))
}

#[derive(Parser)]
#[command(name = "ws-ckpt", version, about = "btrfs workspace snapshot manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Arguments for `ws-ckpt config`. Scope (-g vs -w) is exclusive but optional;
/// when omitted, the command renders an overview (global + ws roll-up) — view-only.
///
/// `--reset` is per-workspace only (deletes that ws's `policy.toml`). The
/// interval/image flags are global-only; using them with `-w` is rejected
/// at runtime with a clear error.
#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("scope").args(["global", "workspace"]).required(false).multiple(false),
))]
struct ConfigArgs {
    /// Operate on /etc/ws-ckpt/config.toml (daemon-wide).
    #[arg(short = 'g', long = "global")]
    global: bool,

    /// Operate on this workspace's policy.toml override.
    #[arg(short = 'w', long = "workspace", value_parser = workspace_value_parser())]
    workspace: Option<String>,

    /// Per-workspace only: delete `policy.toml`, restoring inherit-global.
    #[arg(long, conflicts_with_all = ["enable_auto_cleanup", "disable_auto_cleanup", "auto_cleanup_keep", "auto_cleanup_interval", "health_check_interval", "img_size", "img_max_percent"])]
    reset: bool,

    /// Set health check interval in seconds (0 disables the scheduler loop).
    /// Global-only.
    #[arg(long)]
    health_check_interval: Option<u64>,

    /// Set target image size in GB (image will be grown/shrunk at next daemon restart).
    /// Global-only.
    #[arg(long)]
    img_size: Option<u64>,

    /// Set initial-creation cap as percentage of host partition (0-100);
    /// only used on first bootstrap. Global-only.
    #[arg(long)]
    img_max_percent: Option<f64>,

    /// Enable periodic auto-cleanup.
    #[arg(long, conflicts_with = "disable_auto_cleanup")]
    enable_auto_cleanup: bool,

    /// Disable periodic auto-cleanup.
    #[arg(long, conflicts_with = "enable_auto_cleanup")]
    disable_auto_cleanup: bool,

    /// Set cleanup retention: integer (count mode, 0 = disabled) or duration
    /// like "30d" (age mode, units s/m/h/d/w).
    #[arg(long, value_parser = parse_cleanup_retention)]
    auto_cleanup_keep: Option<CleanupRetention>,

    /// Set auto-cleanup interval in seconds (0 disables the scheduler loop).
    /// Global-only — `-w` callers passing this are rejected.
    #[arg(long)]
    auto_cleanup_interval: Option<u64>,

    /// Output format: `text` (human-readable, default) or `json`. Programmatic
    /// consumers should use `json` — text is not a contract; the JSON shape is
    /// versioned by its `schema` field.
    #[arg(long, default_value = "text")]
    format: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the ws-ckpt daemon (manual mode)
    Daemon {
        /// btrfs filesystem mount point
        #[arg(long, default_value = DEFAULT_MOUNT_PATH)]
        mount_path: PathBuf,

        /// Unix socket path for IPC
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: PathBuf,

        /// Log level (debug/info/warn/error)
        #[arg(long, default_value = "info")]
        log_level: String,
    },

    /// Initialize a workspace for btrfs snapshot management
    Init {
        /// Workspace path or ID (absolute path, relative path, or workspace ID)
        #[arg(long, short = 'w', value_parser = workspace_value_parser())]
        workspace: String,
    },

    /// Create a checkpoint (readonly snapshot)
    Checkpoint {
        /// Workspace path or ID (absolute path, relative path, or workspace ID)
        #[arg(long, short = 'w', value_parser = workspace_value_parser())]
        workspace: String,

        /// Snapshot ID (auto-generated if omitted)
        #[arg(long = "snapshot", short = 's', conflicts_with = "legacy_id", value_parser = snapshot_id_value_parser())]
        snapshot: Option<String>,

        #[arg(long = "id", short = 'i', hide = true, value_parser = snapshot_id_value_parser())]
        legacy_id: Option<String>,

        /// Commit message describing the checkpoint
        #[arg(long, short = 'm')]
        message: Option<String>,

        /// Additional metadata as JSON string
        #[arg(long)]
        metadata: Option<String>,
    },

    /// Rollback workspace to a specific snapshot
    Rollback {
        /// Workspace path or ID (absolute path, relative path, or workspace ID)
        #[arg(long, short = 'w', value_parser = workspace_value_parser())]
        workspace: String,

        /// Target snapshot (ID or prefix) — mutually exclusive with -n
        #[arg(long = "snapshot", short = 's', conflicts_with = "num_ancestors", value_parser = snapshot_id_value_parser())]
        to: Option<String>,

        /// Roll back N ancestors along parent chain — mutually exclusive with -s
        #[arg(long = "num-ancestors", short = 'n', conflicts_with = "to", value_parser = clap::value_parser!(u32).range(1..))]
        num_ancestors: Option<u32>,

        /// Preview file changes without executing rollback
        #[arg(long)]
        preview: bool,
    },

    /// Delete a specific snapshot
    Delete {
        /// Workspace path or ID (optional; omit for global snapshot lookup)
        #[arg(long, short = 'w', value_parser = workspace_value_parser())]
        workspace: Option<String>,

        /// Complete snapshot ID (prefixes are not accepted)
        #[arg(long, short = 's', value_parser = snapshot_id_value_parser())]
        snapshot: String,

        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },

    /// List snapshots for a workspace (or all workspaces if omitted)
    List {
        /// Workspace path or ID (optional; omit to list all workspaces)
        #[arg(long, short = 'w', value_parser = workspace_value_parser())]
        workspace: Option<String>,

        /// Only snapshots recovered without their original metadata
        #[arg(long)]
        orphans: bool,

        /// Output format: table or json (default: table)
        #[arg(long, default_value = "table", value_parser = ["table", "json"])]
        format: String,

        /// Maximum snapshots to return in one page
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=MAX_LIST_PAGE_ITEMS as i64))]
        limit: Option<u32>,

        /// Opaque continuation token returned by a previous page
        #[arg(long)]
        cursor: Option<String>,
    },

    /// Show diff between two snapshots, or between a snapshot and the current workspace
    Diff {
        /// Workspace path or ID (absolute path, relative path, or workspace ID)
        #[arg(long, short = 'w', value_parser = workspace_value_parser())]
        workspace: String,

        /// Source snapshot (ID or name)
        #[arg(long, short = 'f', value_parser = snapshot_id_value_parser())]
        from: String,

        /// Target snapshot (ID or name); omit to diff against current workspace
        #[arg(long, short = 't', value_parser = snapshot_id_value_parser())]
        to: Option<String>,
    },

    /// Show daemon and workspace status
    Status {
        /// Workspace path or ID (optional filter)
        #[arg(long, short = 'w', value_parser = workspace_value_parser())]
        workspace: Option<String>,

        /// Output format: table or json (default: table)
        #[arg(long, default_value = "table", value_parser = ["table", "json"])]
        format: String,
    },

    /// Clean up old snapshots, keeping the most recent ones
    Cleanup {
        /// Workspace path or ID (absolute path, relative path, or workspace ID)
        #[arg(long, short = 'w', value_parser = workspace_value_parser())]
        workspace: String,

        /// Number of recent unpinned snapshots to keep (default: 20)
        #[arg(long, default_value = "20")]
        keep: u32,
    },

    /// View or update daemon / per-workspace configuration.
    ///
    /// **Scope is required for any modification**: pass exactly one of
    /// `-g/--global` or `-w/--workspace` when setting flags or `--reset`.
    /// No scope = read-only overview (global cfg + per-ws override count);
    /// no scope + flags = hard error, so it's always obvious which layer
    /// a write lands on.
    Config(ConfigArgs),

    /// Trigger daemon to reload /etc/ws-ckpt/config.toml
    Reload,

    /// Recover workspace to a normal directory (undo init)
    Recover {
        /// Workspace path or ID
        #[arg(short, long, conflicts_with = "all", value_parser = workspace_value_parser())]
        workspace: Option<String>,

        /// Recover all registered workspaces
        #[arg(long, conflicts_with = "workspace")]
        all: bool,

        /// Skip interactive confirmation
        #[arg(long)]
        force: bool,
    },

    /// Remove a stale registration when its live subvolume is missing (no data restored)
    Unregister {
        /// Registered workspace path or ID
        #[arg(short, long, value_parser = workspace_value_parser())]
        workspace: String,
        /// Skip interactive confirmation
        #[arg(long)]
        force: bool,
    },

    /// Install or uninstall the ws-ckpt plugin for an agent runtime
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
}

#[derive(Subcommand, Debug)]
enum PluginAction {
    /// Install the ws-ckpt plugin into an agent runtime
    Install {
        /// Target runtime
        #[arg(long, short, default_value = "openclaw")]
        runtime: PluginRuntime,
    },
    /// Uninstall the ws-ckpt plugin from an agent runtime
    Uninstall {
        /// Target runtime
        #[arg(long, short, default_value = "openclaw")]
        runtime: PluginRuntime,
    },
}

#[derive(clap::ValueEnum, Clone, Debug)]
enum PluginRuntime {
    Openclaw,
    Hermes,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let is_daemon = matches!(cli.command, Commands::Daemon { .. });
    let start = std::time::Instant::now();

    match run(cli).await {
        Ok(()) => {
            if !is_daemon {
                let elapsed = start.elapsed().as_secs_f64();
                // JSON mode: inject timing into buffered output; text mode: stderr.
                if let Some(mut data) = take_json_output() {
                    if let serde_json::Value::Object(map) = &mut data {
                        map.insert("elapsed_secs".to_string(), serde_json::json!(elapsed));
                    }
                    println!("{}", serde_json::to_string_pretty(&data).unwrap());
                } else {
                    println!("Completed in {:.3}s", elapsed);
                }
            }
            // Post-command soft advisory: best-effort, silent on failure.
            print_health_advisory_if_needed().await;
        }
        Err(e) => {
            let elapsed = start.elapsed().as_secs_f64();
            if let Some(mut data) = take_json_output() {
                if let serde_json::Value::Object(map) = &mut data {
                    map.insert("elapsed_secs".to_string(), serde_json::json!(elapsed));
                    map.insert("error".to_string(), serde_json::json!(format!("{:#}", e)));
                }
                // Errors go to stderr so stdout stays clean for pipes.
                eprintln!("{}", serde_json::to_string_pretty(&data).unwrap());
            } else if !is_daemon {
                eprintln!("Failed after {:.3}s", elapsed);
            }
            if !is_daemon {
                eprintln!("\x1b[31mError: {:#}\x1b[0m", e);
            } else {
                // Surface failure reason in daemon mode (captured by journald).
                eprintln!("ws-ckpt daemon failed to start: {:#}", e);
            }
            process::exit(1);
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Daemon {
            mount_path,
            socket,
            log_level,
        } => {
            // Load auto-cleanup settings from config file
            let file_config = load_config_file(std::path::Path::new(CONFIG_FILE_PATH))
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Warning: failed to load {}: {}, using defaults",
                        CONFIG_FILE_PATH, e
                    );
                    Default::default()
                });
            let config = DaemonConfig {
                mount_path,
                socket_path: socket,
                log_level,
                auto_cleanup: file_config.auto_cleanup.unwrap_or(DEFAULT_AUTO_CLEANUP),
                auto_cleanup_keep: file_config
                    .auto_cleanup_keep
                    .clone()
                    .unwrap_or_else(default_auto_cleanup_keep),
                auto_cleanup_interval_secs: file_config
                    .auto_cleanup_interval_secs
                    .unwrap_or(DEFAULT_AUTO_CLEANUP_INTERVAL_SECS),
                health_check_interval_secs: file_config
                    .health_check_interval_secs
                    .unwrap_or(DEFAULT_HEALTH_CHECK_INTERVAL_SECS),
                backend_type: file_config.backend.r#type.clone(),
                img_size: file_config
                    .backend
                    .btrfs_loop
                    .as_ref()
                    .and_then(|b| b.img_size)
                    .unwrap_or(DEFAULT_IMG_SIZE_GB),
                img_max_percent: file_config
                    .backend
                    .btrfs_loop
                    .as_ref()
                    .and_then(|b| b.img_max_percent)
                    .unwrap_or(DEFAULT_IMG_MAX_PERCENT * 100.0),
                min_free_bytes: 512 * 1024 * 1024,
                min_free_percent: 1.0,
            };
            ws_ckpt_daemon::run_daemon(config).await?;
        }
        Commands::Init { workspace } => {
            let workspace = resolve_workspace_arg(&workspace);
            // Catch a plain missing path here so the caller gets that answer
            // instead of the daemon's namespace-scoped one, which has to hedge
            // about shared volumes. symlink_metadata, not exists(): a dangling
            // workspace symlink is the daemon's re-init recovery path. Only
            // NotFound is conclusive — the daemon is privileged and may reach
            // paths this process cannot stat, so any other error goes to it.
            if let Err(e) = std::fs::symlink_metadata(&workspace) {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::bail!("path does not exist: {}", workspace);
                }
            }
            let request = Request::Init { workspace };
            let response = send_request_to_daemon(&request).await?;
            handle_response(response, &request).await?;
        }
        Commands::Checkpoint {
            workspace,
            snapshot,
            legacy_id,
            message,
            metadata,
        } => {
            let id = match (snapshot, legacy_id) {
                (_, Some(legacy)) => {
                    eprintln!("Warning: --id/-i is deprecated, use --snapshot/-s instead");
                    legacy
                }
                (Some(s), _) => s,
                (None, None) => generate_auto_id(),
            };
            // Validate metadata is valid JSON if provided
            if let Some(ref s) = metadata {
                serde_json::from_str::<serde_json::Value>(s)
                    .context("invalid JSON in --metadata")?;
            }
            let request = Request::Checkpoint {
                workspace: resolve_workspace_arg(&workspace),
                id,
                message,
                metadata,
                pin: false,
            };
            let response = send_request_to_daemon(&request).await?;
            handle_response(response, &request).await?;
        }
        Commands::Rollback {
            workspace,
            to,
            num_ancestors,
            preview,
        } => {
            if to.is_none() && num_ancestors.is_none() {
                anyhow::bail!("either --snapshot/-s or --num-ancestors/-n must be specified");
            }
            if preview {
                let request = Request::RollbackPreview {
                    workspace: resolve_workspace_arg(&workspace),
                    to,
                    num_ancestors,
                };
                let response = send_request_to_daemon(&request).await?;
                handle_rollback_preview_response(response)?;
            } else {
                let request = Request::Rollback {
                    workspace: resolve_workspace_arg(&workspace),
                    to,
                    num_ancestors,
                };
                let response = send_request_to_daemon(&request).await?;
                handle_response(response, &request).await?;
            }
        }
        Commands::Delete {
            workspace,
            snapshot,
            force,
        } => {
            let request = Request::Delete {
                workspace: workspace.as_deref().map(resolve_workspace_arg),
                snapshot,
                force,
            };
            let response = send_request_to_daemon(&request).await?;
            handle_response(response, &request).await?;
        }
        Commands::List {
            workspace,
            format,
            orphans,
            limit,
            cursor,
        } => {
            let workspace = workspace.as_deref().map(resolve_workspace_arg);
            let explicit_page = limit.is_some() || cursor.is_some();
            let page_limit = limit.unwrap_or(DEFAULT_LIST_PAGE_LIMIT);
            if explicit_page {
                let request = Request::ListPage {
                    orphans_only: orphans,
                    workspace,
                    limit: page_limit,
                    cursor,
                };
                let response = send_request_to_daemon(&request).await?;
                handle_list_page_response(response, &format, true)?;
            } else {
                let mut all = Vec::new();
                let mut next_cursor = None;
                loop {
                    let request = Request::ListPage {
                        orphans_only: orphans,
                        workspace: workspace.clone(),
                        limit: page_limit,
                        cursor: next_cursor.take(),
                    };
                    let response = send_request_to_daemon(&request).await?;
                    next_cursor = accept_list_page(&mut all, response)?;
                    if next_cursor.is_none() {
                        break;
                    }
                }
                render_snapshot_list(all, None, &format, false)?;
            }
        }
        Commands::Diff {
            workspace,
            from,
            to,
        } => {
            let request = Request::Diff {
                workspace: resolve_workspace_arg(&workspace),
                from,
                to,
            };

            let response = send_request_to_daemon(&request).await?;
            handle_diff_response(response)?;
        }
        Commands::Status { workspace, format } => {
            let request = Request::Status {
                workspace: workspace.as_deref().map(resolve_workspace_arg),
            };
            let response = send_request_to_daemon(&request).await?;
            handle_status_response(response, &format)?;
        }
        Commands::Cleanup { workspace, keep } => {
            let request = Request::Cleanup {
                workspace: resolve_workspace_arg(&workspace),
                keep: Some(keep),
            };
            let response = send_request_to_daemon(&request).await?;
            handle_cleanup_response(response)?;
        }
        Commands::Config(args) => {
            handle_config_command(args).await?;
        }
        Commands::Reload => {
            handle_reload().await?;
        }
        Commands::Recover {
            workspace,
            all,
            force,
        } => {
            handle_recover(workspace, all, force).await?;
        }
        Commands::Unregister { workspace, force } => {
            handle_unregister(&workspace, force).await?;
        }
        Commands::Plugin { action } => {
            handle_plugin(action)?;
        }
    }
    Ok(())
}

/// Get the socket path from env or default
fn get_socket_path() -> PathBuf {
    std::env::var("WS_CKPT_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_SOCKET_PATH))
}

/// Clap value parser that rejects empty and whitespace-only workspace strings.
/// Stricter than `NonEmptyStringValueParser`, which only rejects `""`.
fn workspace_value_parser() -> impl TypedValueParser<Value = String> {
    StringValueParser::new().try_map(|s: String| {
        if s.trim().is_empty() {
            Err("workspace argument must not be empty or whitespace")
        } else {
            Ok(s)
        }
    })
}

/// Snapshot id becomes a path component; blanks and `/\`/`.`/`..` would
/// produce records the lookup paths can't address.
fn snapshot_id_value_parser() -> impl TypedValueParser<Value = String> {
    StringValueParser::new().try_map(|s: String| {
        if s.trim().is_empty() {
            return Err("snapshot id must not be empty or whitespace");
        }
        if s.contains('/') || s.contains('\\') || s == "." || s == ".." {
            return Err("snapshot id must not contain path separators or be '.'/'..'");
        }
        Ok(s)
    })
}

fn generate_auto_id() -> String {
    chrono::Utc::now()
        .format("ckpt-%Y%m%dT%H%M%S%.3f")
        .to_string()
}

const ADAPTER_RELATIVE_ROOT: &str = "anolisa/adapters/ws-ckpt";
const SYSTEM_ADAPTER_DATA_DIRS: [&str; 2] = ["/usr/share", "/usr/local/share"];

fn plugin_adapter_roots(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = SYSTEM_ADAPTER_DATA_DIRS
        .iter()
        .map(|data_dir| PathBuf::from(data_dir).join(ADAPTER_RELATIVE_ROOT))
        .collect::<Vec<_>>();
    if let Some(home) = home {
        roots.push(home.join(".local/share").join(ADAPTER_RELATIVE_ROOT));
    }
    roots
}

fn resolve_plugin_adapter_dir(
    runtime_dir: &str,
    required_scripts: &[&str],
    adapter_roots: &[PathBuf],
) -> Result<PathBuf> {
    let candidates = adapter_roots
        .iter()
        .map(|root| root.join(runtime_dir))
        .collect::<Vec<_>>();

    if let Some(candidate) = candidates.iter().find(|candidate| {
        required_scripts
            .iter()
            .all(|script| candidate.join(script).is_file())
    }) {
        return Ok(candidate.clone());
    }

    let searched = candidates
        .iter()
        .flat_map(|candidate| {
            required_scripts
                .iter()
                .map(move |script| format!("  - {}", candidate.join(script).display()))
        })
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::bail!(
        "cannot find a complete ws-ckpt {runtime_dir} adapter; searched:\n{searched}\nis ws-ckpt installed?"
    )
}

fn handle_plugin(action: PluginAction) -> Result<()> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let adapter_roots = plugin_adapter_roots(home.as_deref());
    handle_plugin_with_adapter_roots(action, &adapter_roots)
}

#[cfg(test)]
fn handle_plugin_with_adapter_root(action: PluginAction, adapter_root: &Path) -> Result<()> {
    handle_plugin_with_adapter_roots(action, &[adapter_root.to_path_buf()])
}

fn handle_plugin_with_adapter_roots(action: PluginAction, adapter_roots: &[PathBuf]) -> Result<()> {
    let (runtime, runtime_dir) = match &action {
        PluginAction::Install { runtime } | PluginAction::Uninstall { runtime } => match runtime {
            PluginRuntime::Openclaw => (runtime, "openclaw"),
            PluginRuntime::Hermes => (runtime, "hermes"),
        },
    };

    let action_script = match &action {
        PluginAction::Install { .. } => format!("install-{runtime_dir}.sh"),
        PluginAction::Uninstall { .. } => format!("uninstall-{runtime_dir}.sh"),
    };
    let detect_script = format!("detect-{runtime_dir}.sh");
    let required_scripts = match (&action, runtime) {
        (PluginAction::Install { .. }, PluginRuntime::Openclaw) => vec![
            detect_script.as_str(),
            action_script.as_str(),
            "lib-discover.sh",
            "lib-openclaw.sh",
        ],
        (PluginAction::Uninstall { .. }, PluginRuntime::Openclaw) => {
            vec![action_script.as_str(), "lib-openclaw.sh"]
        }
        (PluginAction::Install { .. }, PluginRuntime::Hermes) => vec![
            detect_script.as_str(),
            action_script.as_str(),
            "lib-discover.sh",
        ],
        (PluginAction::Uninstall { .. }, PluginRuntime::Hermes) => {
            vec![action_script.as_str()]
        }
    };
    let adapter_dir = resolve_plugin_adapter_dir(runtime_dir, &required_scripts, adapter_roots)?;

    if let PluginAction::Install { .. } = &action {
        let detect_script = adapter_dir.join(detect_script);
        let detect_code = std::process::Command::new("bash")
            .arg(&detect_script)
            .status()
            .with_context(|| format!("failed to run {}", detect_script.display()))?
            .code()
            .unwrap_or(-1);
        match detect_code {
            0 => match runtime {
                PluginRuntime::Openclaw => {
                    // Reinstall also reconciles the tool allowlist after configuration drift.
                    eprintln!("openclaw plugin already installed; refreshing configuration");
                }
                PluginRuntime::Hermes => {
                    eprintln!("hermes plugin already installed");
                    return Ok(());
                }
            },
            1 => {}
            2 => anyhow::bail!("missing prerequisites for {runtime:?}"),
            _ => anyhow::bail!("detect failed for {runtime:?} (exit {detect_code})"),
        }
    }

    let script_path = adapter_dir.join(&action_script);
    let status = std::process::Command::new("bash")
        .arg(&script_path)
        .status()
        .with_context(|| format!("failed to run {}", script_path.display()))?;

    if !status.success() {
        let verb = match action {
            PluginAction::Install { .. } => "install",
            PluginAction::Uninstall { .. } => "uninstall",
        };
        anyhow::bail!(
            "{verb} failed for {runtime:?} (exit {})",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

/// Resolve workspace identifier: convert filesystem paths to absolute,
/// pass workspace IDs through unchanged.
///
/// IMPORTANT: We must NOT follow symlinks here. With symlink-based workspaces,
/// the user-facing path is a symlink (e.g. `/tmp/test-ws -> /mnt/btrfs-workspace/ws-xxx`).
/// The daemon registers the symlink path, so we must preserve it.
///
/// Assumes the input has already been validated non-empty by
/// `workspace_value_parser`; callers feeding raw strings should validate first.
fn resolve_workspace_arg(workspace: &str) -> String {
    let path = std::path::Path::new(workspace);
    // If it looks like a workspace ID (no path separators), pass through unchanged
    if !workspace.contains('/') && !workspace.contains('\\') {
        return workspace.to_string();
    }
    // Convert to absolute path WITHOUT following symlinks
    if path.is_absolute() {
        workspace.to_string()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(workspace).to_string_lossy().to_string(),
            Err(_) => workspace.to_string(),
        }
    }
}

/// Send a request to the daemon via Unix Socket and receive a response
async fn send_request_to_daemon(request: &Request) -> Result<Response> {
    let socket_path = get_socket_path();

    let mut stream = UnixStream::connect(&socket_path).await.map_err(|e| {
        match e.kind() {
            std::io::ErrorKind::NotFound => {
                eprintln!("\x1b[31m\u{2717} Daemon is not running.\x1b[0m");
                eprintln!("  Start the systemd service `ws-ckpt`, or its daemon container.");
                process::exit(1);
            }
            std::io::ErrorKind::ConnectionRefused => {
                eprintln!(
                    "\x1b[33m\u{26a0} Daemon is starting up. Please retry in a few seconds.\x1b[0m"
                );
                process::exit(1);
            }
            _ => {}
        }
        anyhow::anyhow!(
            "Failed to connect to ws-ckpt daemon ({}): {}",
            socket_path.display(),
            e
        )
    })?;

    // Send request
    let frame = encode_frame(request)?;
    stream
        .write_all(&frame)
        .await
        .context("failed to send request")?;

    // Read response: 4-byte LE length + payload
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("failed to read response length")?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_SIZE {
        anyhow::bail!(
            "Response frame too large: {} bytes (max {})",
            len,
            MAX_FRAME_SIZE
        );
    }
    let mut payload = vec![0u8; len as usize];
    stream
        .read_exact(&mut payload)
        .await
        .context("failed to read response payload")?;

    let response: Response = decode_payload(&payload)?;
    Ok(response)
}

/// Best-effort pre-view reload. `None` = global only; `Some(ws)` = global + that ws.
async fn try_reload_daemon_for_view(workspace: Option<&str>) {
    silent_reload(&Request::ReloadGlobalConfig).await;
    if let Some(ws) = workspace {
        silent_reload(&Request::ReloadWorkspacePolicy {
            workspace: resolve_workspace_arg(ws),
        })
        .await;
    }
}

async fn silent_reload(req: &Request) {
    match try_send_request_to_daemon_silent(req).await {
        Ok(Response::ReloadConfigOk { .. }) | Err(_) => {}
        Ok(Response::Error { message, .. }) => {
            eprintln!(
                "\x1b[33m\u{26a0} View may be stale: daemon reload failed: {}\x1b[0m",
                message
            );
        }
        Ok(_) => {}
    }
}

/// Silent IPC: errors bubble up so the caller can ignore them; never `process::exit`.
async fn try_send_request_to_daemon_silent(request: &Request) -> Result<Response> {
    let socket_path = get_socket_path();

    let mut stream = UnixStream::connect(&socket_path)
        .await
        .context("connect to daemon (silent)")?;

    let frame = encode_frame(request)?;
    stream
        .write_all(&frame)
        .await
        .context("send request (silent)")?;

    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("read response length (silent)")?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_SIZE {
        anyhow::bail!(
            "Response frame too large: {} bytes (max {})",
            len,
            MAX_FRAME_SIZE
        );
    }
    let mut payload = vec![0u8; len as usize];
    stream
        .read_exact(&mut payload)
        .await
        .context("read response payload (silent)")?;

    let response: Response = decode_payload(&payload)?;
    Ok(response)
}

/// Emit a best-effort advisory to stderr when any threshold is crossed.
/// Silent on timeout/IPC error; `fs_total_bytes == 0` skips only the fs warning.
async fn print_health_advisory_if_needed() {
    let (over_limit_workspace_count, fs_total_bytes, fs_used_bytes) = match tokio::time::timeout(
        ADVISORY_TIMEOUT,
        try_send_request_to_daemon_silent(&Request::HealthAdvisory),
    )
    .await
    {
        Ok(Ok(Response::HealthAdvisoryOk {
            over_limit_workspace_count,
            fs_total_bytes,
            fs_used_bytes,
        })) => (over_limit_workspace_count, fs_total_bytes, fs_used_bytes),
        _ => return,
    };

    let snapshot_warning: Option<String> = if over_limit_workspace_count > 0 {
        Some(format!(
            "{} workspace(s) have more than {} snapshots. Run `ws-ckpt status` to see details",
            over_limit_workspace_count, ADVISORY_SNAPSHOT_LIMIT
        ))
    } else {
        None
    };

    let fs_warning: Option<String> = if fs_total_bytes > 0 {
        let pct = (fs_used_bytes as f64 / fs_total_bytes as f64) * 100.0;
        if pct > ADVISORY_FS_USAGE_PCT {
            Some(format!(
                "btrfs backend usage above {}% (current: {:.1}%)",
                ADVISORY_FS_USAGE_PCT, pct
            ))
        } else {
            None
        }
    } else {
        None
    };

    if snapshot_warning.is_none() && fs_warning.is_none() {
        return;
    }

    eprintln!();
    if let Some(msg) = &snapshot_warning {
        eprintln!("\x1b[33m\u{26a0} Warning: {}.\x1b[0m", msg);
    }
    if let Some(msg) = &fs_warning {
        eprintln!("\x1b[33m\u{26a0} Warning: {}.\x1b[0m", msg);
    }
    eprintln!();
    eprintln!("\x1b[33m  Manual cleanup:   ws-ckpt cleanup -w <workspace> --keep <N>\x1b[0m");
    eprintln!("\x1b[33m  Or enable auto-cleanup for all workspaces:\x1b[0m");
    eprintln!("\x1b[33m      ws-ckpt config -g --enable-auto-cleanup \\\x1b[0m");
    eprintln!("\x1b[33m                        --auto-cleanup-keep <NUM|DURATION> \\\x1b[0m");
    eprintln!("\x1b[33m                        --auto-cleanup-interval <SECONDS>\x1b[0m");
    eprintln!("\x1b[33m  Suggested values:\x1b[0m");
    eprintln!("\x1b[33m      ws-ckpt config -g --enable-auto-cleanup --auto-cleanup-keep 1000 --auto-cleanup-interval 86400\x1b[0m");
}

/// Handle the response, printing formatted output.
/// If ConfirmationRequired, prompt user and re-send with force=true.
async fn handle_response(response: Response, original_request: &Request) -> Result<()> {
    match response {
        Response::InitOk { ws_id } => {
            println!("\x1b[32m✓ Workspace initialized: {}\x1b[0m", ws_id);
        }
        Response::CheckpointOk { snapshot_id } => {
            println!("\x1b[32m✓ Checkpoint created: {}\x1b[0m", snapshot_id);
        }
        Response::RollbackOk { from, to } => {
            println!(
                "\x1b[32m✓ Rolled back workspace {} to snapshot: {}\x1b[0m",
                from, to
            );
        }
        Response::DeleteOk { target } => {
            println!("\x1b[32m✓ Deleted: {}\x1b[0m", target);
        }
        Response::RecoverWithWarning { workspace, warning } => {
            eprintln!("WARNING: {}", warning);
            println!("Workspace recovered: {}", workspace);
        }
        Response::RecoverOk { workspace } => {
            println!("\x1b[32m\u{2713} Workspace recovered: {}\x1b[0m", workspace);
        }
        Response::Error {
            code: ErrorCode::ConfirmationRequired,
            message,
        } => {
            eprintln!("\x1b[33m⚠ {}\x1b[0m", message);
            eprint!("确认操作? [y/N]: ");
            io::stderr().flush()?;

            let stdin = io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;

            if line.trim().eq_ignore_ascii_case("y") {
                // Re-send with force=true
                let force_request = match original_request {
                    Request::Delete {
                        workspace,
                        snapshot,
                        ..
                    } => Request::Delete {
                        workspace: workspace.clone(),
                        snapshot: snapshot.clone(),
                        force: true,
                    },
                    _ => {
                        anyhow::bail!("unexpected ConfirmationRequired for non-delete request");
                    }
                };
                let response = send_request_to_daemon(&force_request).await?;
                Box::pin(handle_response(response, &force_request)).await?;
            } else {
                println!("操作已取消");
            }
        }
        Response::Error {
            code: ErrorCode::SnapshotAlreadyExists,
            message,
        } => {
            eprintln!("\x1b[31m✗ {}\x1b[0m", message);
            eprintln!("  Please use a different snapshot ID.");
            process::exit(1);
        }
        Response::Error {
            code: ErrorCode::DiskSpaceInsufficient,
            message,
        } => {
            eprintln!("\x1b[31m\u{2717} {}\x1b[0m", message);
            eprintln!("  Use 'ws-ckpt delete' to remove old snapshots, or free disk space.");
            process::exit(1);
        }
        Response::Error {
            code: ErrorCode::SnapshotNotFound,
            message,
        } => {
            eprintln!("\x1b[31m✗ {}\x1b[0m", message);
            eprintln!("  Use 'ws-ckpt list' to view available snapshots.");
            process::exit(1);
        }
        Response::Error {
            code: ErrorCode::WorkspaceNotFound,
            message,
        } => {
            eprintln!("\x1b[31m✗ {}\x1b[0m", message);
            eprintln!("  Use 'ws-ckpt init' to initialize, or 'ws-ckpt list' to view workspaces.");
            process::exit(1);
        }
        Response::Error {
            code: ErrorCode::CwdOccupied,
            message,
        } => {
            eprintln!("\x1b[31m✗ {}\x1b[0m", message);
            eprintln!(
                "  Tip: inspect each PID above with `ps -fp <PID>`, then cd them out or kill."
            );
            eprintln!(
                "  (cwd may be a bind-mount alias of the workspace, not the workspace path.)"
            );
            process::exit(1);
        }
        Response::Error {
            code: ErrorCode::CwdScanFailed,
            message,
        } => {
            eprintln!("\x1b[31m✗ {}\x1b[0m", message);
            eprintln!("  This is typically transient — retry the command.");
            process::exit(1);
        }
        Response::Error { code, message } => {
            eprintln!("\x1b[31mError [{:?}]: {}\x1b[0m", code, message);
            process::exit(1);
        }
        Response::CheckpointSkipped { reason } => {
            eprintln!("\x1b[33m\u{26a0} {}\x1b[0m", reason);
        }
        _ => {
            // Phase 2 responses are handled by dedicated functions
            eprintln!("\x1b[33mUnexpected response type\x1b[0m");
        }
    }
    Ok(())
}

fn accept_list_page(all: &mut Vec<SnapshotListItem>, response: Response) -> Result<Option<String>> {
    match response {
        Response::ListPageOk {
            snapshots,
            next_cursor,
        } => {
            all.extend(snapshots);
            Ok(next_cursor)
        }
        Response::Error { code, message } => anyhow::bail!("list failed [{code:?}]: {message}"),
        _ => anyhow::bail!("unexpected response type while listing snapshots"),
    }
}

fn snapshot_item_json(item: &SnapshotListItem) -> Result<serde_json::Value> {
    match item {
        SnapshotListItem::Full(entry) => Ok(serde_json::to_value(entry)?),
        SnapshotListItem::Summary(summary) => Ok(serde_json::json!({
            "id": summary.id,
            "workspace": summary.workspace,
            "meta": {
                "created_at": summary.created_at,
                "pinned": summary.pinned,
                "missing": summary.missing,
            },
            "detail": "summary",
            "omitted_fields": summary.omitted_fields,
        })),
    }
}

fn list_item_workspace(item: &SnapshotListItem) -> &str {
    match item {
        SnapshotListItem::Full(entry) => &entry.workspace,
        SnapshotListItem::Summary(summary) => &summary.workspace,
    }
}

fn render_snapshot_list(
    snapshots: Vec<SnapshotListItem>,
    next_cursor: Option<String>,
    format: &str,
    explicit_page: bool,
) -> Result<()> {
    if format == "json" {
        let values = snapshots
            .iter()
            .map(snapshot_item_json)
            .collect::<Result<Vec<_>>>()?;
        if explicit_page {
            emit_json(serde_json::json!({
                "snapshots": values,
                "next_cursor": next_cursor,
            }));
        } else {
            emit_json(serde_json::Value::Array(values));
        }
        return Ok(());
    }

    if snapshots.is_empty() {
        println!("No snapshots found.");
        if let Some(cursor) = next_cursor {
            println!("Next cursor: {cursor}");
        }
        return Ok(());
    }

    let hdr_ws = "WORKSPACE";
    let hdr_snap = "SNAPSHOT";
    let offset_secs = chrono::Local::now().offset().local_minus_utc();
    let sign = if offset_secs >= 0 { '+' } else { '-' };
    let h = offset_secs.abs() / 3600;
    let m = (offset_secs.abs() % 3600) / 60;
    let local_offset = if m == 0 {
        format!("{sign}{h}")
    } else {
        format!("{sign}{h}:{m:02}")
    };
    let hdr_date_owned = format!("CREATED (UTC{local_offset})");
    let hdr_date = hdr_date_owned.as_str();
    let hdr_msg = "MESSAGE";
    let id_len = |item: &SnapshotListItem| match item {
        SnapshotListItem::Full(entry) => {
            entry.id.len() + usize::from(entry.meta.missing) * " [MISSING]".len()
        }
        SnapshotListItem::Summary(summary) => {
            summary.id.len()
                + usize::from(summary.missing) * " [MISSING]".len()
                + " [SUMMARY]".len()
        }
    };
    let w_ws = snapshots
        .iter()
        .map(|item| list_item_workspace(item).len())
        .max()
        .unwrap_or(0)
        .max(hdr_ws.len());
    let w_snap = snapshots
        .iter()
        .map(id_len)
        .max()
        .unwrap_or(0)
        .max(hdr_snap.len());
    let w_date = 19_usize.max(hdr_date.len());
    println!(
        "{:<w_ws$} {:<w_snap$} {:<w_date$} PINNED {}",
        hdr_ws, hdr_snap, hdr_date, hdr_msg,
    );
    println!(
        "{}",
        "-".repeat(w_ws + w_snap + w_date + hdr_msg.len() + 10)
    );
    for item in &snapshots {
        let (workspace, id, created_at, pinned, message) = match item {
            SnapshotListItem::Full(entry) => (
                entry.workspace.as_str(),
                if entry.meta.missing {
                    format!("{} [MISSING]", entry.id)
                } else {
                    entry.id.clone()
                },
                entry.meta.created_at,
                entry.meta.pinned,
                entry.meta.message.as_deref().unwrap_or("-").to_string(),
            ),
            SnapshotListItem::Summary(summary) => (
                summary.workspace.as_str(),
                format!(
                    "{}{} [SUMMARY]",
                    summary.id,
                    if summary.missing { " [MISSING]" } else { "" }
                ),
                summary.created_at,
                summary.pinned,
                format!("details omitted: {}", summary.omitted_fields.join(", ")),
            ),
        };
        println!(
            "{:<w_ws$} {:<w_snap$} {:<w_date$} {:<6} {}",
            workspace,
            id,
            created_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S"),
            pinned,
            message,
        );
    }
    println!("\nTotal: {} snapshot(s)", snapshots.len());
    if let Some(cursor) = next_cursor {
        println!("More snapshots are available. Next cursor: {cursor}");
    }
    Ok(())
}

fn handle_list_page_response(response: Response, format: &str, explicit_page: bool) -> Result<()> {
    match response {
        Response::ListPageOk {
            snapshots,
            next_cursor,
        } => render_snapshot_list(snapshots, next_cursor, format, explicit_page),
        Response::Error { code, message } => anyhow::bail!("list failed [{code:?}]: {message}"),
        _ => anyhow::bail!("unexpected response type while listing snapshots"),
    }
}

/// Handle DiffOk response, formatting diff entries.
fn handle_diff_response(response: Response) -> Result<()> {
    match response {
        Response::DiffOk { changes } => {
            print_diff_entries(&changes);
        }
        Response::Error { code, message } => {
            eprintln!("\x1b[31mError [{:?}]: {}\x1b[0m", code, message);
            process::exit(1);
        }
        _ => {
            eprintln!("\x1b[33mUnexpected response type\x1b[0m");
        }
    }
    Ok(())
}

/// Handle RollbackPreviewOk response, formatting the rollback diff summary.
fn handle_rollback_preview_response(response: Response) -> Result<()> {
    match response {
        Response::RollbackPreviewOk { to, changes } => {
            println!("\x1b[1mRollback preview (no changes applied)\x1b[0m");
            println!("Target snapshot: {}\n", to);
            println!("Changes since target snapshot (rollback will revert these):");
            print_diff_entries(&changes);
        }
        Response::Error { code, message } => {
            eprintln!("\x1b[31mError [{:?}]: {}\x1b[0m", code, message);
            process::exit(1);
        }
        _ => {
            eprintln!("\x1b[33mUnexpected response type\x1b[0m");
        }
    }
    Ok(())
}

fn print_diff_entries(changes: &[ws_ckpt_common::DiffEntry]) {
    if changes.is_empty() {
        println!("No differences found.");
        return;
    }

    for entry in changes {
        let marker = match entry.change_type {
            ChangeType::Added => "\x1b[32m+",
            ChangeType::Deleted => "\x1b[31m-",
            ChangeType::Modified => "\x1b[33mM",
            ChangeType::Renamed => "\x1b[36mR",
        };
        let detail = entry
            .detail
            .as_deref()
            .map(|d| format!(" ({})", d))
            .unwrap_or_default();
        println!("{}  {}{}\x1b[0m", marker, entry.path, detail);
    }
    println!("\n{} change(s)", changes.len());
}

/// Handle StatusOk response, formatting the status report.
fn handle_status_response(response: Response, format: &str) -> Result<()> {
    match response {
        Response::StatusOk { report } => {
            if format == "json" {
                emit_json(serde_json::to_value(&report)?);
            } else {
                println!("\x1b[1mDaemon Status\x1b[0m");
                println!("  Uptime: {} seconds", report.uptime_secs);
                println!(
                    "  Filesystem: {:.2} GB used / {:.2} GB total",
                    report.fs_used_bytes as f64 / 1_073_741_824.0,
                    report.fs_total_bytes as f64 / 1_073_741_824.0,
                );
                println!();
                if report.workspaces.is_empty() {
                    println!("  No workspaces registered.");
                } else {
                    println!("  {:<25} {:<40} SNAPSHOTS", "WORKSPACE", "PATH");
                    println!("  {}", "-".repeat(75));
                    for ws in &report.workspaces {
                        println!("  {:<25} {:<40} {}", ws.ws_id, ws.path, ws.snapshot_count);
                    }
                    println!("\n  Total: {} workspace(s)", report.workspaces.len());
                }
            }
        }
        Response::Error { code, message } => {
            eprintln!("\x1b[31mError [{:?}]: {}\x1b[0m", code, message);
            process::exit(1);
        }
        _ => {
            eprintln!("\x1b[33mUnexpected response type\x1b[0m");
        }
    }
    Ok(())
}

/// Handle CleanupOk response.
fn handle_cleanup_response(response: Response) -> Result<()> {
    match response {
        Response::CleanupOk { removed } => {
            if removed.is_empty() {
                println!("\x1b[32m\u{2713} No snapshots needed cleanup.\x1b[0m");
            } else {
                println!(
                    "\x1b[32m\u{2713} Cleaned up {} snapshot(s):\x1b[0m",
                    removed.len()
                );
                for snap in &removed {
                    println!("  - {}", snap);
                }
            }
        }
        Response::Error { code, message } => {
            eprintln!("\x1b[31mError [{:?}]: {}\x1b[0m", code, message);
            process::exit(1);
        }
        _ => {
            eprintln!("\x1b[33mUnexpected response type\x1b[0m");
        }
    }
    Ok(())
}

/// Top-level dispatcher for `ws-ckpt config`. Decides global vs per-ws based
/// on the (clap-enforced) ArgGroup, then routes to the appropriate handler.
async fn handle_config_command(args: ConfigArgs) -> Result<()> {
    let format = parse_output_format(&args.format)?;
    let auto_cleanup = match (args.enable_auto_cleanup, args.disable_auto_cleanup) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    let any_update = auto_cleanup.is_some()
        || args.auto_cleanup_keep.is_some()
        || args.auto_cleanup_interval.is_some()
        || args.health_check_interval.is_some()
        || args.img_size.is_some()
        || args.img_max_percent.is_some();

    if args.global {
        if args.reset {
            anyhow::bail!("--reset is only valid with -w/--workspace");
        }
        if any_update {
            handle_global_config_update(
                args.health_check_interval,
                args.img_size,
                args.img_max_percent,
                auto_cleanup,
                args.auto_cleanup_keep,
                args.auto_cleanup_interval,
                format,
            )
            .await?;
        } else {
            handle_global_config_view(format).await?;
        }
        return Ok(());
    }

    // No scope: overview (view-only). Updates need an explicit scope.
    if args.workspace.is_none() {
        if args.reset || any_update {
            anyhow::bail!("specify -g (global) or -w <ws> (per-workspace) to make changes");
        }
        handle_config_overview_view(format).await?;
        return Ok(());
    }

    let ws = args
        .workspace
        .clone()
        .expect("workspace is Some in this branch");

    if args.reset {
        handle_workspace_config_reset(&ws, format).await?;
        return Ok(());
    }

    // Per-ws: reject global-only fields up front so the user gets a clear
    // error instead of a silently dropped flag.
    if args.auto_cleanup_interval.is_some() {
        anyhow::bail!("--auto-cleanup-interval is global-only; use `-g` (interval is daemon-wide)");
    }
    if args.health_check_interval.is_some() {
        anyhow::bail!("--health-check-interval is global-only; use `-g`");
    }
    if args.img_size.is_some() {
        anyhow::bail!("--img-size is global-only; use `-g`");
    }
    if args.img_max_percent.is_some() {
        anyhow::bail!("--img-max-percent is global-only; use `-g`");
    }

    if !any_update {
        handle_workspace_config_view(&ws, format).await?;
    } else {
        handle_workspace_config_update(&ws, auto_cleanup, args.auto_cleanup_keep, format).await?;
    }
    Ok(())
}

/// Per-ws view: queries the daemon for `effective / local / global` and
/// renders all three columns (or JSON).
async fn handle_workspace_config_view(workspace: &str, format: OutputFormat) -> Result<()> {
    // Align daemon snapshot with on-disk config.toml + this ws's policy.toml.
    try_reload_daemon_for_view(Some(workspace)).await;
    let req = Request::GetWorkspacePolicy {
        workspace: resolve_workspace_arg(workspace),
    };
    let resp = send_request_to_daemon(&req).await?;
    print_workspace_policy_response_formatted(resp, format)
}

/// Per-ws update: send each user-mentioned field as `PolicyFieldOp::Set`,
/// leave the rest `Unchanged`. The daemon does the read-modify-write
/// atomically under the per-ws lock (no CLI-side GET), so concurrent
/// `--enable-auto-cleanup` and `--auto-cleanup-keep N` can't lose updates.
async fn handle_workspace_config_update(
    workspace: &str,
    auto_cleanup: Option<bool>,
    auto_cleanup_keep: Option<CleanupRetention>,
    format: OutputFormat,
) -> Result<()> {
    let asked_enable = auto_cleanup == Some(true);
    let supplied_keep = auto_cleanup_keep.is_some();
    let req = Request::PatchWorkspacePolicy {
        workspace: resolve_workspace_arg(workspace),
        auto_cleanup: match auto_cleanup {
            Some(v) => PolicyFieldOp::Set(v),
            None => PolicyFieldOp::Unchanged,
        },
        auto_cleanup_keep: match auto_cleanup_keep {
            Some(v) => PolicyFieldOp::Set(v),
            None => PolicyFieldOp::Unchanged,
        },
    };
    let resp = send_request_to_daemon(&req).await?;

    if let Response::WorkspacePolicyOk {
        ref effective,
        ref local,
        ..
    } = resp
    {
        if let Some(msg) = disabled_warning_for(asked_enable, supplied_keep, effective, local) {
            eprintln!("\x1b[33m\u{26a0} Warning: {}\x1b[0m", msg);
        }
    }
    print_workspace_policy_response_formatted(resp, format)
}

/// Warn when the user touched any policy field but the result is still
/// effective-disabled. Names whichever layer(s) cause it (cleanup off,
/// keep=0, or both), so the fix hint points at the right knob.
fn disabled_warning_for(
    asked_enable: bool,
    supplied_keep: bool,
    effective: &ws_ckpt_common::EffectivePolicy,
    local: &ws_ckpt_common::WorkspacePolicy,
) -> Option<String> {
    if !(asked_enable || supplied_keep) || !effective.is_disabled() {
        return None;
    }
    let mut reasons: Vec<String> = Vec::new();
    if !effective.auto_cleanup {
        let local_off = local.auto_cleanup == Some(false);
        reasons.push(if local_off {
            "local auto_cleanup is false (override with `--enable-auto-cleanup` on this command)".to_string()
        } else {
            "global auto_cleanup is false (pass `--enable-auto-cleanup` here for a per-ws override, or `ws-ckpt config -g --enable-auto-cleanup` to flip it globally)".to_string()
        });
    }
    if effective.auto_cleanup_keep.is_disabled() {
        let local_keep_off = local
            .auto_cleanup_keep
            .as_ref()
            .map(|k| k.is_disabled())
            .unwrap_or(false);
        reasons.push(if local_keep_off {
            format!(
                "local auto_cleanup_keep is {} (override with `--auto-cleanup-keep <N>`, N >= 1)",
                format_retention(local.auto_cleanup_keep.as_ref().unwrap())
            )
        } else {
            format!(
                "global auto_cleanup_keep is {} (pass `--auto-cleanup-keep <N>` here, or `ws-ckpt config -g --auto-cleanup-keep N`)",
                format_retention(&effective.auto_cleanup_keep)
            )
        });
    }
    Some(format!(
        "auto-cleanup is still effectively disabled — {}.",
        reasons.join("; ")
    ))
}

/// Per-ws reset: delete `policy.toml` for this workspace.
async fn handle_workspace_config_reset(workspace: &str, format: OutputFormat) -> Result<()> {
    let req = Request::ResetWorkspacePolicy {
        workspace: resolve_workspace_arg(workspace),
    };
    let resp = send_request_to_daemon(&req).await?;
    print_workspace_policy_response_formatted(resp, format)
}

fn print_policy_field(name: &str, effective: &str, local: Option<String>, global: &str) {
    let local_str = match local {
        Some(s) => s,
        None => "(inherit)".to_string(),
    };
    println!(
        "  {:<22} effective={:<14} local={:<14} global={}",
        name, effective, local_str, global
    );
}

fn format_retention(r: &CleanupRetention) -> String {
    match r {
        CleanupRetention::Count(n) => n.to_string(),
        CleanupRetention::Age { raw, .. } => format!("\"{}\"", raw),
    }
}

// ── Output format flag ──────────────────────────────────────────────────
//
// The JSON output struct types themselves live in `ws_ckpt_common`
// (mirrors how `SnapshotEntry` / `ConfigReport` etc. live there and are
// dumped by `list --format json` / `status --format json`). Keeping the
// schemas there means:
//   - cli stays free of a direct `serde` dep (derive happens in common)
//   - future Rust tools / tests can share the same versioned schema
//     definitions without round-tripping JSON

/// Output-format flag value. A typo like `--format jso` is rejected at the
/// CLI boundary rather than silently falling back to text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
}

fn parse_output_format(s: &str) -> Result<OutputFormat> {
    match s {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        other => anyhow::bail!(
            "unknown --format value: {:?} (expected `text` or `json`)",
            other
        ),
    }
}

/// Print a `Response::WorkspacePolicyOk` (or surface a daemon error)
/// according to the requested output format.
fn print_workspace_policy_response_formatted(resp: Response, format: OutputFormat) -> Result<()> {
    match resp {
        Response::WorkspacePolicyOk {
            ws_id,
            effective,
            local,
            global,
        } => match format {
            OutputFormat::Text => {
                println!("\x1b[1mWorkspace policy: {}\x1b[0m", ws_id);
                print_policy_field(
                    "auto_cleanup",
                    &effective.auto_cleanup.to_string(),
                    local.auto_cleanup.map(|b| b.to_string()),
                    &global.auto_cleanup.to_string(),
                );
                print_policy_field(
                    "auto_cleanup_keep",
                    &format_retention(&effective.auto_cleanup_keep),
                    local.auto_cleanup_keep.as_ref().map(format_retention),
                    &format_retention(&global.auto_cleanup_keep),
                );
                println!(
                    "  auto_cleanup_interval: (global-only) — see `ws-ckpt config -g` to view"
                );
                Ok(())
            }
            OutputFormat::Json => {
                let json = WorkspacePolicyJson::from_views(ws_id, &effective, &local, &global);
                emit_json(serde_json::to_value(&json)?);
                Ok(())
            }
        },
        Response::Error { code, message } => {
            anyhow::bail!("[{:?}] {}", code, message);
        }
        other => anyhow::bail!("Unexpected response from daemon: {:?}", other),
    }
}

/// View current global configuration. Triggers a best-effort daemon reload
/// first so what we read from disk also matches what daemon is using; if the
/// daemon is offline we still render from file (no daemon required).
async fn handle_global_config_view(format: OutputFormat) -> Result<()> {
    try_reload_daemon_for_view(None).await;
    let path = std::path::Path::new(CONFIG_FILE_PATH);
    let fc = load_config_file(path).map_err(|e| anyhow::anyhow!("Failed to read config: {}", e))?;

    let auto_cleanup = fc.auto_cleanup.unwrap_or(DEFAULT_AUTO_CLEANUP);
    let keep = fc
        .auto_cleanup_keep
        .clone()
        .unwrap_or_else(default_auto_cleanup_keep);
    let interval = fc
        .auto_cleanup_interval_secs
        .unwrap_or(DEFAULT_AUTO_CLEANUP_INTERVAL_SECS);
    let health = fc
        .health_check_interval_secs
        .unwrap_or(DEFAULT_HEALTH_CHECK_INTERVAL_SECS);
    let btrfs_loop = fc.backend.btrfs_loop.as_ref();
    let img_size = btrfs_loop
        .and_then(|b| b.img_size)
        .unwrap_or(DEFAULT_IMG_SIZE_GB);
    let img_max = btrfs_loop
        .and_then(|b| b.img_max_percent)
        .unwrap_or(DEFAULT_IMG_MAX_PERCENT * 100.0);

    if format == OutputFormat::Json {
        let json = GlobalConfigJson {
            schema: GLOBAL_CONFIG_JSON_SCHEMA,
            config_file: CONFIG_FILE_PATH.to_string(),
            mount_path: DEFAULT_MOUNT_PATH.to_string(),
            socket_path: DEFAULT_SOCKET_PATH.to_string(),
            auto_cleanup,
            auto_cleanup_keep: (&keep).into(),
            auto_cleanup_is_disabled: !auto_cleanup || keep.is_disabled(),
            auto_cleanup_interval_secs: interval,
            health_check_interval_secs: health,
            img_size_gb: img_size,
            img_max_percent: img_max,
        };
        emit_json(serde_json::to_value(&json)?);
        return Ok(());
    }

    let keep_display = match &keep {
        CleanupRetention::Count(n) => format!("{} (count mode)", n),
        CleanupRetention::Age { raw, .. } => format!("\"{}\" (age mode)", raw),
    };

    println!("\x1b[1mDaemon Configuration\x1b[0m");
    println!("  Config file:             {}", CONFIG_FILE_PATH);
    println!(
        "  Mount path:              {} (default)",
        DEFAULT_MOUNT_PATH
    );
    println!(
        "  Socket path:             {} (default)",
        DEFAULT_SOCKET_PATH
    );
    println!(
        "  Auto-cleanup:            {}{}",
        if auto_cleanup { "enabled" } else { "disabled" },
        if fc.auto_cleanup.is_none() {
            " (default)"
        } else {
            ""
        }
    );
    println!(
        "  Auto-cleanup keep:       {}{}",
        keep_display,
        if fc.auto_cleanup_keep.is_none() {
            " (default)"
        } else {
            ""
        }
    );
    println!(
        "  Auto-cleanup interval:   {}s ({}m){}",
        interval,
        interval / 60,
        if fc.auto_cleanup_interval_secs.is_none() {
            " (default)"
        } else {
            ""
        }
    );
    println!(
        "  Health-check interval:   {}s ({}m){}",
        health,
        health / 60,
        if fc.health_check_interval_secs.is_none() {
            " (default)"
        } else {
            ""
        }
    );
    println!(
        "  Image size:              {} GB{}",
        img_size,
        if btrfs_loop.and_then(|b| b.img_size).is_none() {
            " (default)"
        } else {
            ""
        }
    );
    println!(
        "  Image max percent:       {}%{}",
        img_max,
        if btrfs_loop.and_then(|b| b.img_max_percent).is_none() {
            " (default)"
        } else {
            ""
        }
    );
    Ok(())
}

/// `ws-ckpt config` (no scope): global cfg + ws roll-up. View-only.
async fn handle_config_overview_view(format: OutputFormat) -> Result<()> {
    try_reload_daemon_for_view(None).await;
    let resp = send_request_to_daemon(&Request::ConfigOverview).await?;
    let (config, ws_total, ws_with_override) = match resp {
        Response::ConfigOverviewOk {
            config,
            ws_total,
            ws_with_override,
        } => (config, ws_total, ws_with_override),
        Response::Error { code, message } => anyhow::bail!("[{:?}] {}", code, message),
        other => anyhow::bail!("Unexpected response from daemon: {:?}", other),
    };
    let inherit = ws_total.saturating_sub(ws_with_override);

    if format == OutputFormat::Json {
        let json = serde_json::json!({
            "schema": OVERVIEW_JSON_SCHEMA,
            "config_file": CONFIG_FILE_PATH,
            "global": config,
            "workspaces": {
                "total": ws_total,
                "with_override": ws_with_override,
                "inherit_global": inherit,
            },
        });
        emit_json(json);
        return Ok(());
    }

    println!("\x1b[1mDaemon Configuration\x1b[0m");
    println!("  Config file:             {}", CONFIG_FILE_PATH);
    println!("  Mount path:              {}", config.mount_path);
    println!("  Socket path:             {}", config.socket_path);
    println!(
        "  Auto-cleanup:            {}",
        if config.auto_cleanup {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  Auto-cleanup keep:       {}",
        format_retention(&config.auto_cleanup_keep)
    );
    println!(
        "  Auto-cleanup interval:   {}s ({}m)",
        config.auto_cleanup_interval_secs,
        config.auto_cleanup_interval_secs / 60
    );
    println!(
        "  Health-check interval:   {}s ({}m)",
        config.health_check_interval_secs,
        config.health_check_interval_secs / 60
    );
    println!("  Image size:              {} GB", config.img_size);
    println!("  Image max percent:       {}%", config.img_max_percent);
    println!();
    println!("\x1b[1mWorkspaces\x1b[0m");
    println!("  Total:                   {}", ws_total);
    println!("  With local override:     {}", ws_with_override);
    println!("  Inherit global:          {}", inherit);
    if ws_with_override > 0 {
        println!("\nUse `ws-ckpt config -w <ws>` to inspect a specific workspace.");
    }
    Ok(())
}

/// Update global configuration: write to config file + notify daemon to reload.
async fn handle_global_config_update(
    health_check_interval: Option<u64>,
    img_size: Option<u64>,
    img_max_percent: Option<f64>,
    auto_cleanup: Option<bool>,
    auto_cleanup_keep: Option<CleanupRetention>,
    auto_cleanup_interval_secs: Option<u64>,
    format: OutputFormat,
) -> Result<()> {
    let path = std::path::Path::new(CONFIG_FILE_PATH);

    // Load existing config
    let mut fc =
        load_config_file(path).map_err(|e| anyhow::anyhow!("Failed to read config: {}", e))?;

    // Apply updates
    if let Some(interval) = health_check_interval {
        fc.health_check_interval_secs = Some(interval);
    }
    if let Some(v) = auto_cleanup {
        fc.auto_cleanup = Some(v);
    }
    if let Some(v) = auto_cleanup_keep {
        fc.auto_cleanup_keep = Some(v);
    }
    if let Some(v) = auto_cleanup_interval_secs {
        fc.auto_cleanup_interval_secs = Some(v);
    }
    // Record whether any btrfs-loop image settings are being changed,
    // as these only take effect at daemon bootstrap (not on reload).
    let has_img_settings = img_size.is_some() || img_max_percent.is_some();

    // Handle btrfs-loop settings
    if has_img_settings {
        let mut bl = fc.backend.btrfs_loop.unwrap_or_default();
        if let Some(s) = img_size {
            bl.img_size = Some(s);
        }
        if let Some(c) = img_max_percent {
            bl.img_max_percent = Some(c);
        }
        fc.backend.btrfs_loop = Some(bl);
    }

    // Save
    save_config_file(path, &fc).map_err(|e| anyhow::anyhow!("Failed to save config: {}", e))?;

    // JSON mode: keep stdout a single parseable payload (the post-update
    // config) by routing status/advisory lines to stderr.
    let status_sink: fn(std::fmt::Arguments<'_>) = if format == OutputFormat::Json {
        |args| eprintln!("{}", args)
    } else {
        |args| println!("{}", args)
    };
    status_sink(format_args!(
        "\x1b[32m\u{2713} Configuration saved to {}\x1b[0m",
        CONFIG_FILE_PATH
    ));

    // Try to notify running daemon. Only the global cfg changed — no need
    // to walk every per-ws policy.toml. The reply carries the post-reload
    // ConfigReport so JSON mode can emit the landed state on stdout
    // without a follow-up `Config` round-trip.
    let mut reloaded_config: Option<ws_ckpt_common::ConfigReport> = None;
    let mut daemon_file: Option<ws_ckpt_common::FileConfig> = None;
    let mut reload_error: Option<String> = None;
    match send_request_to_daemon(&Request::ReloadGlobalConfig).await {
        Ok(Response::ReloadConfigOk { config, file }) => {
            status_sink(format_args!(
                "\x1b[32m\u{2713} Daemon reloaded configuration\x1b[0m"
            ));
            reloaded_config = Some(config);
            daemon_file = file;
        }
        Ok(Response::Error { message, .. }) => {
            reload_error = Some(format!("daemon reload failed: {}", message));
        }
        Err(_) => {
            status_sink(format_args!(
                "\x1b[33m\u{26a0} Daemon not running, changes will take effect on next start\x1b[0m"
            ));
        }
        _ => {}
    }

    // The daemon reloads CONFIG_FILE_PATH as *it* sees it. When CLI and daemon
    // do not share that path — k8s sidecar puts them in separate container
    // filesystems — the reload succeeds against an absent file, the daemon
    // falls back to built-in defaults, and the policy silently never applies.
    // Compare the file the daemon parsed against the one just written so that
    // case surfaces as a failure instead of a green checkmark. Comparing files
    // rather than effective values also covers the bootstrap-only image
    // settings, which no post-reload runtime view can confirm.
    if let Some(landed) = &daemon_file {
        let unlanded = unlanded_global_settings(&fc, landed);
        if !unlanded.is_empty() {
            reload_error = Some(format!(
                "daemon did not load the settings written to {}:\n  {}\n\
                 The daemon reloads its own view of that path — if it runs in a \
                 separate container or mount namespace, {} must be on a volume \
                 shared with this CLI.",
                CONFIG_FILE_PATH,
                unlanded.join("\n  "),
                CONFIG_FILE_PATH
            ));
        }
    }

    // Only advise a restart once the file is known to have reached the daemon;
    // otherwise the restart would reload the same unseen file.
    if has_img_settings && reload_error.is_none() && reloaded_config.is_some() {
        status_sink(format_args!(
            "\x1b[33m\u{26a0} Note: btrfs-loop image settings (img-size, img-max-percent) require daemon restart to take effect.\x1b[0m"
        ));
    }

    // JSON mode emits the post-update state on stdout so callers don't need
    // a follow-up read. Prefer the daemon's freshly-reloaded ConfigReport
    // (single source of truth); fall back to a local view only if the
    // daemon wasn't reachable.
    if format == OutputFormat::Json {
        match reloaded_config {
            Some(cr) => {
                let json = config_report_to_global_json(&cr);
                emit_json(serde_json::to_value(&json)?);
            }
            None => handle_global_config_view(format).await?,
        }
    }

    if let Some(err) = reload_error {
        anyhow::bail!("{}", err);
    }

    Ok(())
}

/// Fields where the config file just written differs from the one the daemon
/// parsed, rendered as `field: file has X, daemon has Y`.
///
/// Compares the two files rather than file-against-effective-runtime so that
/// `img_size` and `img_max_percent` are covered too: those apply at bootstrap
/// only, so no post-reload runtime view can tell a daemon that read them from
/// one that never saw the file.
fn unlanded_global_settings(
    saved: &ws_ckpt_common::FileConfig,
    landed: &ws_ckpt_common::FileConfig,
) -> Vec<String> {
    /// Renders an unset optional as `unset` so the two sides stay comparable.
    fn shown<T: std::fmt::Display>(v: Option<T>) -> String {
        v.map_or_else(|| "unset".to_string(), |v| v.to_string())
    }

    // Whole-struct equality is the gate, not the per-field list below. A field
    // added to FileConfig later is caught by `==` before anyone remembers to
    // extend this function; the per-field lines only make the failure legible.
    if saved == landed {
        return Vec::new();
    }

    let mut unlanded = Vec::new();
    if saved.auto_cleanup != landed.auto_cleanup {
        unlanded.push(format!(
            "auto-cleanup: file has {}, daemon has {}",
            shown(saved.auto_cleanup),
            shown(landed.auto_cleanup)
        ));
    }
    if saved.auto_cleanup_keep != landed.auto_cleanup_keep {
        unlanded.push(format!(
            "auto-cleanup-keep: file has {}, daemon has {}",
            shown(saved.auto_cleanup_keep.as_ref().map(format_retention)),
            shown(landed.auto_cleanup_keep.as_ref().map(format_retention))
        ));
    }
    if saved.auto_cleanup_interval_secs != landed.auto_cleanup_interval_secs {
        unlanded.push(format!(
            "auto-cleanup-interval: file has {}, daemon has {}",
            shown(saved.auto_cleanup_interval_secs),
            shown(landed.auto_cleanup_interval_secs)
        ));
    }
    if saved.health_check_interval_secs != landed.health_check_interval_secs {
        unlanded.push(format!(
            "health-check-interval: file has {}, daemon has {}",
            shown(saved.health_check_interval_secs),
            shown(landed.health_check_interval_secs)
        ));
    }
    if saved.backend.r#type != landed.backend.r#type {
        unlanded.push(format!(
            "backend type: file has {}, daemon has {}",
            saved.backend.r#type, landed.backend.r#type
        ));
    }
    let saved_loop = saved.backend.btrfs_loop.as_ref();
    let landed_loop = landed.backend.btrfs_loop.as_ref();
    let (saved_size, landed_size) = (
        saved_loop.and_then(|b| b.img_size),
        landed_loop.and_then(|b| b.img_size),
    );
    if saved_size != landed_size {
        unlanded.push(format!(
            "img-size: file has {}, daemon has {}",
            shown(saved_size),
            shown(landed_size)
        ));
    }
    let (saved_pct, landed_pct) = (
        saved_loop.and_then(|b| b.img_max_percent),
        landed_loop.and_then(|b| b.img_max_percent),
    );
    if saved_pct != landed_pct {
        unlanded.push(format!(
            "img-max-percent: file has {}, daemon has {}",
            shown(saved_pct),
            shown(landed_pct)
        ));
    }
    // The `==` gate above already proved the two differ; if none of the known
    // fields account for it, the difference is in a field this build does not
    // know about. Still surface something rather than an empty list.
    if unlanded.is_empty() {
        unlanded.push("config file contents differ (unrecognized field)".to_string());
    }
    unlanded
}

/// Render a daemon-returned [`ConfigReport`] as the same [`GlobalConfigJson`]
/// schema that `config -g --format json` emits. Used by `-g update` to emit
/// the post-reload state on stdout without a follow-up `Config` IPC.
fn config_report_to_global_json(cr: &ws_ckpt_common::ConfigReport) -> GlobalConfigJson {
    GlobalConfigJson {
        schema: GLOBAL_CONFIG_JSON_SCHEMA,
        config_file: CONFIG_FILE_PATH.to_string(),
        mount_path: cr.mount_path.clone(),
        socket_path: cr.socket_path.clone(),
        auto_cleanup: cr.auto_cleanup,
        auto_cleanup_keep: (&cr.auto_cleanup_keep).into(),
        auto_cleanup_is_disabled: !cr.auto_cleanup || cr.auto_cleanup_keep.is_disabled(),
        auto_cleanup_interval_secs: cr.auto_cleanup_interval_secs,
        health_check_interval_secs: cr.health_check_interval_secs,
        img_size_gb: cr.img_size,
        img_max_percent: cr.img_max_percent,
    }
}

/// Handle `ws-ckpt reload` (also used by systemd `ExecReload=`): send
/// `Request::ReloadConfig` IPC. If the daemon is not running,
/// `send_request_to_daemon` exits 1 with a red message.
async fn handle_reload() -> Result<()> {
    match send_request_to_daemon(&Request::ReloadConfig).await? {
        Response::ReloadConfigOk { .. } => {
            println!("\x1b[32m\u{2713} Daemon reloaded configuration\x1b[0m");
            Ok(())
        }
        Response::Error { code, message } => {
            anyhow::bail!("Daemon reload failed [{:?}]: {}", code, message);
        }
        other => anyhow::bail!("Unexpected response from daemon: {:?}", other),
    }
}

async fn handle_unregister(workspace: &str, force: bool) -> Result<()> {
    let workspace = resolve_workspace_arg(workspace);
    if !force {
        println!("Remove the registration for {} only if its live subvolume is missing. No data will be restored; snapshots, backups, and snapshot metadata are retained.", workspace);
        eprint!("Proceed? [y/N] ");
        io::stderr().flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        if !matches!(line.trim(), "y" | "Y") {
            println!("Operation cancelled.");
            return Ok(());
        }
    }
    match send_request_to_daemon(&Request::Unregister { workspace }).await? {
        Response::UnregisterOk {
            workspace,
            retained_paths,
        } => {
            println!(
                "Registration removed: {}. The live subvolume was missing; no data was restored.",
                workspace
            );
            println!(
                "Retained recovery locations (if present): {}",
                retained_paths.join(", ")
            );
            println!("Restore or create the workspace directory before running init again.");
        }
        Response::Error { code, message } => anyhow::bail!("Error [{:?}]: {}", code, message),
        response => anyhow::bail!("unexpected unregister response: {:?}", response),
    }
    Ok(())
}

// Every destructive recovery uses daemon-resolved metadata, including --force.
async fn preview_recovery(workspace: String) -> Result<RecoveryPreview> {
    let response = send_request_to_daemon(&Request::RecoverPreview { workspace })
        .await
        .context("Cannot preview recovery; ensure the daemon supports recovery preview")?;
    recovery_preview_from_response(response)
}

fn recovery_preview_from_response(response: Response) -> Result<RecoveryPreview> {
    match response {
        Response::RecoverPreviewOk { preview } => Ok(preview),
        Response::Error { code, message } => {
            anyhow::bail!("Recovery preview failed [{code:?}]: {message}")
        }
        _ => anyhow::bail!("Daemon did not return a recovery preview; upgrade/restart the daemon before recovering"),
    }
}

/// Handle recover command: single workspace or all workspaces.
async fn handle_recover(workspace: Option<String>, all: bool, force: bool) -> Result<()> {
    if workspace.is_none() && !all {
        eprintln!("\x1b[31mError: either --workspace/-w or --all must be specified\x1b[0m");
        process::exit(1);
    }

    if all {
        // Batch mode: get all workspaces via Status
        let status_req = Request::Status { workspace: None };
        let status_resp = send_request_to_daemon(&status_req).await?;
        let workspaces = match status_resp {
            Response::StatusOk { report } => report.workspaces,
            Response::Error { code, message } => {
                eprintln!("\x1b[31mError [{:?}]: {}\x1b[0m", code, message);
                process::exit(1);
            }
            _ => {
                eprintln!("\x1b[33mUnexpected response type\x1b[0m");
                process::exit(1);
            }
        };

        if workspaces.is_empty() {
            println!("No workspaces to recover.");
            return Ok(());
        }

        let mut previews = Vec::with_capacity(workspaces.len());
        for ws in &workspaces {
            previews.push(preview_recovery(ws.ws_id.clone()).await?);
        }
        if !force {
            println!("Recovering {} workspace(s):", previews.len());
            for preview in &previews {
                println!(
                    "  {} ({} snapshots)",
                    preview.registration_path, preview.snapshot_count
                );
            }
            println!(
                "This will delete all snapshots and restore all workspaces to normal directories.\n\
                 WARNING: ws-ckpt does NOT check for processes with cwd inside any workspace before recover.\n\
                 Any such process will have its working directory silently invalidated — verify yourself\n\
                 (e.g. lsof +D <ws>, or ls -l /proc/*/cwd) before confirming."
            );
            eprint!("Proceed? [y/N] ");
            io::stderr().flush()?;
            let stdin = io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            if line.trim() != "y" && line.trim() != "Y" {
                println!("Operation cancelled.");
                return Ok(());
            }
        }

        let mut failed: usize = 0;
        for preview in &previews {
            let req = Request::RecoverConfirmed {
                preview: preview.clone(),
            };
            let resp = send_request_to_daemon(&req).await?;
            match resp {
                Response::RecoverWithWarning { workspace, warning } => {
                    eprintln!("WARNING: {}", warning);
                    println!("Workspace recovered: {}", workspace);
                }
                Response::RecoverOk { workspace } => {
                    println!("Workspace recovered: {}", workspace);
                }
                Response::Error { code, message } => {
                    eprintln!(
                        "\x1b[31mError [{:?}] recovering {}: {}\x1b[0m",
                        code, preview.registration_path, message
                    );
                    failed += 1;
                }
                _ => {
                    eprintln!(
                        "\x1b[33mUnexpected response for {}\x1b[0m",
                        preview.registration_path
                    );
                    failed += 1;
                }
            }
        }
        if failed == 0 {
            println!("All workspaces recovered.");
        } else {
            // RPM %preun and other automation chain destructive cleanup off
            // this exit code; a partially failed batch must not look successful.
            let summary = format!(
                "Recover failed for {}/{} workspace(s); failed workspaces \
                 and their snapshots are preserved for retry.",
                failed,
                workspaces.len(),
            );
            eprintln!("\x1b[31m{}\x1b[0m", summary);
            process::exit(1);
        }
    } else {
        // Single workspace mode
        let ws_arg = resolve_workspace_arg(workspace.as_deref().unwrap());

        let preview = preview_recovery(ws_arg).await?;

        if !force {
            println!(
                "Workspace: {} ({} snapshots)",
                preview.registration_path, preview.snapshot_count
            );
            println!(
                "This restores a registered workspace and deletes its snapshots. For interrupted, unregistered init, it restores the pre-init backup and retains migrated storage for inspection.\n\
                 WARNING: ws-ckpt does NOT check for processes with cwd inside the workspace before recover.\n\
                 Any such process will have its working directory silently invalidated — verify yourself\n\
                 (e.g. lsof +D <ws>, or ls -l /proc/*/cwd) before confirming."
            );
            eprint!("Proceed? [y/N] ");
            io::stderr().flush()?;
            let stdin = io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            if line.trim() != "y" && line.trim() != "Y" {
                println!("Operation cancelled.");
                return Ok(());
            }
        }

        let req = Request::RecoverConfirmed { preview };
        let resp = send_request_to_daemon(&req).await?;
        match resp {
            Response::RecoverWithWarning { workspace, warning } => {
                eprintln!("WARNING: {}", warning);
                println!("Workspace recovered: {}", workspace);
            }
            Response::RecoverOk { workspace } => {
                println!("\x1b[32m\u{2713} Workspace recovered: {}\x1b[0m", workspace);
            }
            Response::Error { code, message } => {
                eprintln!("\x1b[31mError [{:?}]: {}\x1b[0m", code, message);
                process::exit(1);
            }
            _ => {
                eprintln!("\x1b[33mUnexpected response type\x1b[0m");
                process::exit(1);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn assert_existing_plugin_install_behavior(
        runtime: PluginRuntime,
        runtime_dir: &str,
        expect_install: bool,
    ) {
        let adapter_root = std::env::temp_dir().join(format!(
            "ws-ckpt-{runtime_dir}-existing-{}",
            std::process::id()
        ));
        let adapter_dir = adapter_root.join(runtime_dir);
        let marker = adapter_dir.join("install-ran");
        let _ = std::fs::remove_dir_all(&adapter_root);
        std::fs::create_dir_all(&adapter_dir).unwrap();
        std::fs::write(
            adapter_dir.join(format!("detect-{runtime_dir}.sh")),
            "exit 0\n",
        )
        .unwrap();
        std::fs::write(
            adapter_dir.join(format!("install-{runtime_dir}.sh")),
            "#!/bin/bash\n: > \"${0%/*}/install-ran\"\n",
        )
        .unwrap();
        std::fs::write(adapter_dir.join("lib-discover.sh"), "").unwrap();
        if let PluginRuntime::Openclaw = &runtime {
            std::fs::write(adapter_dir.join("lib-openclaw.sh"), "").unwrap();
        }

        let result =
            handle_plugin_with_adapter_root(PluginAction::Install { runtime }, &adapter_root);
        let install_ran = marker.is_file();
        let _ = std::fs::remove_dir_all(&adapter_root);

        result.unwrap();
        assert_eq!(install_ran, expect_install);
    }

    #[test]
    fn openclaw_install_refreshes_an_existing_plugin() {
        assert_existing_plugin_install_behavior(PluginRuntime::Openclaw, "openclaw", true);
    }

    #[test]
    fn hermes_install_keeps_existing_plugin() {
        assert_existing_plugin_install_behavior(PluginRuntime::Hermes, "hermes", false);
    }

    #[test]
    fn plugin_adapter_roots_cover_supported_install_locations() {
        assert_eq!(
            plugin_adapter_roots(Some(Path::new("/home/test"))),
            vec![
                PathBuf::from("/usr/share/anolisa/adapters/ws-ckpt"),
                PathBuf::from("/usr/local/share/anolisa/adapters/ws-ckpt"),
                PathBuf::from("/home/test/.local/share/anolisa/adapters/ws-ckpt"),
            ]
        );
    }

    #[test]
    fn plugin_adapter_resolution_uses_first_complete_candidate() {
        let test_root =
            std::env::temp_dir().join(format!("ws-ckpt-adapter-resolution-{}", std::process::id()));
        let first = test_root.join("first");
        let second = test_root.join("second");
        let first_runtime = first.join("openclaw");
        let second_runtime = second.join("openclaw");
        let _ = std::fs::remove_dir_all(&test_root);
        std::fs::create_dir_all(&first_runtime).unwrap();
        std::fs::create_dir_all(&second_runtime).unwrap();
        std::fs::write(first_runtime.join("install-openclaw.sh"), "").unwrap();
        std::fs::write(second_runtime.join("detect-openclaw.sh"), "").unwrap();
        std::fs::write(second_runtime.join("install-openclaw.sh"), "").unwrap();

        let roots = vec![first.clone(), second.clone()];
        let scripts = ["detect-openclaw.sh", "install-openclaw.sh"];
        assert_eq!(
            resolve_plugin_adapter_dir("openclaw", &scripts, &roots).unwrap(),
            second_runtime
        );

        std::fs::write(first_runtime.join("detect-openclaw.sh"), "").unwrap();
        assert_eq!(
            resolve_plugin_adapter_dir("openclaw", &scripts, &roots).unwrap(),
            first_runtime
        );
        std::fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn plugin_install_skips_adapter_missing_sourced_helper() {
        let test_root = std::env::temp_dir().join(format!(
            "ws-ckpt-adapter-helper-resolution-{}",
            std::process::id()
        ));
        let first_runtime = test_root.join("first/openclaw");
        let second_runtime = test_root.join("second/openclaw");
        let _ = std::fs::remove_dir_all(&test_root);

        for adapter_dir in [&first_runtime, &second_runtime] {
            std::fs::create_dir_all(adapter_dir).unwrap();
            std::fs::write(adapter_dir.join("detect-openclaw.sh"), "exit 1\n").unwrap();
            std::fs::write(
                adapter_dir.join("install-openclaw.sh"),
                "#!/bin/bash\n: > \"${0%/*}/install-ran\"\n",
            )
            .unwrap();
            std::fs::write(adapter_dir.join("lib-discover.sh"), "").unwrap();
        }
        std::fs::write(second_runtime.join("lib-openclaw.sh"), "").unwrap();

        let roots = vec![test_root.join("first"), test_root.join("second")];
        let result = handle_plugin_with_adapter_roots(
            PluginAction::Install {
                runtime: PluginRuntime::Openclaw,
            },
            &roots,
        );
        let first_install_ran = first_runtime.join("install-ran").is_file();
        let second_install_ran = second_runtime.join("install-ran").is_file();
        let _ = std::fs::remove_dir_all(&test_root);

        result.unwrap();
        assert!(!first_install_ran);
        assert!(second_install_ran);
    }

    #[test]
    fn plugin_adapter_resolution_reports_all_searched_paths() {
        let roots = vec![PathBuf::from("/first"), PathBuf::from("/second")];
        let scripts = ["detect-hermes.sh", "install-hermes.sh"];
        let error = resolve_plugin_adapter_dir("hermes", &scripts, &roots)
            .unwrap_err()
            .to_string();

        for expected in [
            "/first/hermes/detect-hermes.sh",
            "/first/hermes/install-hermes.sh",
            "/second/hermes/detect-hermes.sh",
            "/second/hermes/install-hermes.sh",
        ] {
            assert!(error.contains(expected), "missing {expected} in {error}");
        }
    }

    // ── Subcommand basic parsing ──

    #[test]
    fn parse_daemon_default() {
        let cli = Cli::try_parse_from(["ws-ckpt", "daemon"]).unwrap();
        match cli.command {
            Commands::Daemon {
                mount_path,
                socket,
                log_level,
            } => {
                assert_eq!(mount_path, PathBuf::from(DEFAULT_MOUNT_PATH));
                assert_eq!(socket, PathBuf::from(DEFAULT_SOCKET_PATH));
                assert_eq!(log_level, "info");
            }
            _ => panic!("expected Daemon"),
        }
    }

    #[test]
    fn parse_init() {
        let cli = Cli::try_parse_from(["ws-ckpt", "init", "--workspace", "/tmp/test"]).unwrap();
        match cli.command {
            Commands::Init { workspace } => assert_eq!(workspace, "/tmp/test"),
            _ => panic!("expected Init"),
        }
    }

    #[test]
    fn parse_rejects_empty_or_whitespace_workspace_on_every_subcommand() {
        let subcommands: &[&[&str]] = &[
            &["init", "-w"],
            &["checkpoint", "-w"],
            &["rollback", "-w"],
            &["delete", "-w"],
            &["list", "-w"],
            &["diff", "-w"],
            &["status", "-w"],
            &["cleanup", "-w"],
            &["recover", "-w"],
        ];
        // Trailing args needed to satisfy required-flag validation for some
        // subcommands. Tested independently for each blank value.
        let trailing: &[(&str, &[&str])] = &[
            ("init", &[]),
            ("checkpoint", &["-s", "snap-1"]),
            ("rollback", &["-s", "snap-1"]),
            ("delete", &["-s", "snap-1"]),
            ("list", &[]),
            ("diff", &["-f", "a", "-t", "b"]),
            ("status", &[]),
            ("cleanup", &[]),
            ("recover", &[]),
        ];
        for blank in ["", "   ", "\t"] {
            for sub in subcommands {
                let name = sub[0];
                let extra = trailing
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, a)| *a)
                    .unwrap_or(&[]);
                let mut argv: Vec<&str> = vec!["ws-ckpt"];
                argv.extend_from_slice(sub);
                argv.push(blank);
                argv.extend_from_slice(extra);
                let err = Cli::try_parse_from(&argv)
                    .err()
                    .unwrap_or_else(|| panic!("expected parse error for argv: {:?}", argv));
                assert_eq!(
                    err.kind(),
                    clap::error::ErrorKind::ValueValidation,
                    "argv {:?} should fail with ValueValidation, got {:?}",
                    argv,
                    err.kind()
                );
            }
        }
    }

    #[test]
    fn parse_accepts_non_blank_workspace() {
        // Sanity: non-blank values still parse.
        Cli::try_parse_from(["ws-ckpt", "init", "-w", "ws-abc"]).unwrap();
        Cli::try_parse_from(["ws-ckpt", "init", "-w", "/foo"]).unwrap();
        Cli::try_parse_from(["ws-ckpt", "init", "-w", "  abc  "]).unwrap();
    }

    #[test]
    fn resolve_workspace_arg_passes_through_non_empty() {
        assert_eq!(resolve_workspace_arg("ws-abc123"), "ws-abc123");
        assert_eq!(resolve_workspace_arg("/abs/path"), "/abs/path");
    }

    #[test]
    fn parse_checkpoint_full() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "checkpoint",
            "--workspace",
            "/tmp/test",
            "--snapshot",
            "msg1-step0",
            "-m",
            "save point",
            "--metadata",
            r#"{"key":"value"}"#,
        ])
        .unwrap();
        match cli.command {
            Commands::Checkpoint {
                workspace,
                snapshot,
                message,
                metadata,
                ..
            } => {
                assert_eq!(workspace, "/tmp/test");
                assert_eq!(snapshot.as_deref(), Some("msg1-step0"));
                assert_eq!(message.as_deref(), Some("save point"));
                assert_eq!(metadata.as_deref(), Some(r#"{"key":"value"}"#));
            }
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn parse_checkpoint_minimal() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "checkpoint",
            "--workspace",
            "/ws",
            "--snapshot",
            "snap-1",
        ])
        .unwrap();
        match cli.command {
            Commands::Checkpoint {
                snapshot,
                message,
                metadata,
                ..
            } => {
                assert_eq!(snapshot.as_deref(), Some("snap-1"));
                assert!(message.is_none());
                assert!(metadata.is_none());
            }
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn parse_checkpoint_short_message_flag() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "checkpoint",
            "--workspace",
            "/ws",
            "--snapshot",
            "snap-1",
            "-m",
            "备注",
        ])
        .unwrap();
        match cli.command {
            Commands::Checkpoint { message, .. } => {
                assert_eq!(message.as_deref(), Some("备注"));
            }
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn parse_rejects_empty_or_whitespace_checkpoint_id() {
        for blank in ["", " ", "   ", "\t"] {
            let err = Cli::try_parse_from(["ws-ckpt", "checkpoint", "-w", "/ws", "-s", blank])
                .err()
                .unwrap_or_else(|| panic!("expected parse error for id {:?}", blank));
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "id {:?} should fail with ValueValidation, got {:?}",
                blank,
                err.kind()
            );
        }
    }

    #[test]
    fn parse_rejects_checkpoint_id_with_path_separators() {
        for bad in ["foo/bar", "..", ".", "a\\b", "/abs"] {
            let err = Cli::try_parse_from(["ws-ckpt", "checkpoint", "-w", "/ws", "-s", bad])
                .err()
                .unwrap_or_else(|| panic!("expected parse error for id {:?}", bad));
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "id {:?} should fail with ValueValidation, got {:?}",
                bad,
                err.kind()
            );
        }
    }

    #[test]
    fn parse_accepts_reasonable_checkpoint_ids() {
        for good in ["snap-1", "msg1-step2", "before-refactor", "v1.2.3"] {
            Cli::try_parse_from(["ws-ckpt", "checkpoint", "-w", "/ws", "-s", good])
                .unwrap_or_else(|_| panic!("expected acceptance for id {:?}", good));
        }
    }

    #[test]
    fn parse_checkpoint_accepts_snapshot_and_legacy_id_flags() {
        // Primary: --snapshot / -s → snapshot field
        let cli =
            Cli::try_parse_from(["ws-ckpt", "checkpoint", "-w", "/ws", "--snapshot", "snap-1"])
                .unwrap();
        match &cli.command {
            Commands::Checkpoint {
                snapshot,
                legacy_id,
                ..
            } => {
                assert_eq!(snapshot.as_deref(), Some("snap-1"));
                assert!(legacy_id.is_none());
            }
            _ => panic!("expected Checkpoint"),
        }

        let cli =
            Cli::try_parse_from(["ws-ckpt", "checkpoint", "-w", "/ws", "-s", "snap-2"]).unwrap();
        match &cli.command {
            Commands::Checkpoint {
                snapshot,
                legacy_id,
                ..
            } => {
                assert_eq!(snapshot.as_deref(), Some("snap-2"));
                assert!(legacy_id.is_none());
            }
            _ => panic!("expected Checkpoint"),
        }

        // Legacy alias: --id / -i → legacy_id field (still parses correctly).
        let cli =
            Cli::try_parse_from(["ws-ckpt", "checkpoint", "-w", "/ws", "--id", "snap-3"]).unwrap();
        match &cli.command {
            Commands::Checkpoint {
                snapshot,
                legacy_id,
                ..
            } => {
                assert!(snapshot.is_none());
                assert_eq!(legacy_id.as_deref(), Some("snap-3"));
            }
            _ => panic!("expected Checkpoint"),
        }

        let cli =
            Cli::try_parse_from(["ws-ckpt", "checkpoint", "-w", "/ws", "-i", "snap-4"]).unwrap();
        match &cli.command {
            Commands::Checkpoint {
                snapshot,
                legacy_id,
                ..
            } => {
                assert!(snapshot.is_none());
                assert_eq!(legacy_id.as_deref(), Some("snap-4"));
            }
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn parse_checkpoint_rejects_both_snapshot_and_id() {
        let result = Cli::try_parse_from([
            "ws-ckpt",
            "checkpoint",
            "-w",
            "/ws",
            "--snapshot",
            "snap-1",
            "--id",
            "snap-2",
        ]);
        assert!(
            result.is_err(),
            "--snapshot and --id should be mutually exclusive"
        );
    }

    // Cases that go through `snapshot_id_value_parser`. Each row is
    // (label, args-with-`SNAP`-placeholder); `SNAP` is replaced per bad value.
    fn snapshot_arg_invocations() -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            (
                "rollback -s",
                vec!["ws-ckpt", "rollback", "-w", "/ws", "-s", "SNAP"],
            ),
            (
                "delete -s",
                vec!["ws-ckpt", "delete", "-w", "/ws", "-s", "SNAP"],
            ),
            (
                "diff -f",
                vec!["ws-ckpt", "diff", "-w", "/ws", "-f", "SNAP", "-t", "ok"],
            ),
            (
                "diff -t",
                vec!["ws-ckpt", "diff", "-w", "/ws", "-f", "ok", "-t", "SNAP"],
            ),
        ]
    }

    #[test]
    fn parse_rejects_empty_or_whitespace_snapshot_args() {
        for (label, template) in snapshot_arg_invocations() {
            for blank in ["", " ", "   ", "\t"] {
                let args: Vec<&str> = template
                    .iter()
                    .map(|a| if *a == "SNAP" { blank } else { *a })
                    .collect();
                let err = Cli::try_parse_from(&args).err().unwrap_or_else(|| {
                    panic!("{}: expected parse error for blank {:?}", label, blank)
                });
                assert_eq!(
                    err.kind(),
                    clap::error::ErrorKind::ValueValidation,
                    "{}: blank {:?} should fail with ValueValidation, got {:?}",
                    label,
                    blank,
                    err.kind()
                );
            }
        }
    }

    #[test]
    fn parse_rejects_path_traversal_snapshot_args() {
        for (label, template) in snapshot_arg_invocations() {
            for bad in ["foo/bar", "..", ".", "a\\b", "/abs"] {
                let args: Vec<&str> = template
                    .iter()
                    .map(|a| if *a == "SNAP" { bad } else { *a })
                    .collect();
                let err = Cli::try_parse_from(&args).err().unwrap_or_else(|| {
                    panic!("{}: expected parse error for value {:?}", label, bad)
                });
                assert_eq!(
                    err.kind(),
                    clap::error::ErrorKind::ValueValidation,
                    "{}: value {:?} should fail with ValueValidation, got {:?}",
                    label,
                    bad,
                    err.kind()
                );
            }
        }
    }

    #[test]
    fn parse_rollback() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "rollback",
            "--workspace",
            "/tmp/test",
            "--snapshot",
            "msg1-step1",
        ])
        .unwrap();
        match cli.command {
            Commands::Rollback {
                workspace,
                to,
                num_ancestors,
                preview,
            } => {
                assert_eq!(workspace, "/tmp/test");
                assert_eq!(to.as_deref(), Some("msg1-step1"));
                assert_eq!(num_ancestors, None);
                assert!(!preview);
            }
            _ => panic!("expected Rollback"),
        }
    }

    #[test]
    fn parse_rollback_num_ancestors() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "rollback",
            "--workspace",
            "/tmp/test",
            "--num-ancestors",
            "3",
        ])
        .unwrap();
        match cli.command {
            Commands::Rollback {
                workspace,
                to,
                num_ancestors,
                preview,
            } => {
                assert_eq!(workspace, "/tmp/test");
                assert_eq!(to, None);
                assert_eq!(num_ancestors, Some(3));
                assert!(!preview);
            }
            _ => panic!("expected Rollback"),
        }
    }

    #[test]
    fn parse_rollback_preview() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "rollback",
            "--workspace",
            "/tmp/test",
            "--snapshot",
            "msg1-step1",
            "--preview",
        ])
        .unwrap();
        match cli.command {
            Commands::Rollback {
                workspace,
                to,
                preview,
                ..
            } => {
                assert_eq!(workspace, "/tmp/test");
                assert_eq!(to.as_deref(), Some("msg1-step1"));
                assert!(preview);
            }
            _ => panic!("expected Rollback"),
        }
    }

    #[test]
    fn parse_delete_default_no_force() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "delete",
            "--workspace",
            "/ws",
            "--snapshot",
            "abc123",
        ])
        .unwrap();
        match cli.command {
            Commands::Delete {
                workspace,
                snapshot,
                force,
            } => {
                assert_eq!(workspace.as_deref(), Some("/ws"));
                assert_eq!(snapshot, "abc123");
                assert!(!force, "force should default to false");
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn parse_delete_with_force() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "delete",
            "-w",
            "/ws",
            "--snapshot",
            "abc123",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Commands::Delete {
                workspace, force, ..
            } => {
                assert_eq!(workspace.as_deref(), Some("/ws"));
                assert!(force);
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn parse_delete_short_snapshot_flag() {
        let cli = Cli::try_parse_from(["ws-ckpt", "delete", "-w", "/ws", "-s", "abc123"]).unwrap();
        match cli.command {
            Commands::Delete {
                workspace,
                snapshot,
                force,
            } => {
                assert_eq!(workspace.as_deref(), Some("/ws"));
                assert_eq!(snapshot, "abc123");
                assert!(!force);
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn delete_missing_snapshot_fails() {
        let result = Cli::try_parse_from(["ws-ckpt", "delete", "--workspace", "/ws"]);
        assert!(result.is_err(), "delete without --snapshot should fail");
    }

    #[test]
    fn delete_without_workspace_parses_ok() {
        let cli = Cli::try_parse_from(["ws-ckpt", "delete", "--snapshot", "abc"]).unwrap();
        match cli.command {
            Commands::Delete {
                workspace,
                snapshot,
                ..
            } => {
                assert!(workspace.is_none());
                assert_eq!(snapshot, "abc");
            }
            _ => panic!("expected Delete"),
        }
    }

    // ── Error cases ──

    #[test]
    fn init_missing_workspace_fails() {
        let result = Cli::try_parse_from(["ws-ckpt", "init"]);
        assert!(result.is_err(), "init without --workspace should fail");
    }

    #[test]
    fn checkpoint_missing_workspace_fails() {
        let result = Cli::try_parse_from(["ws-ckpt", "checkpoint", "--snapshot", "snap-1"]);
        assert!(
            result.is_err(),
            "checkpoint without --workspace should fail"
        );
    }

    #[test]
    fn checkpoint_missing_snapshot_parses_as_none() {
        let cli = Cli::try_parse_from(["ws-ckpt", "checkpoint", "--workspace", "/ws"]).unwrap();
        match cli.command {
            Commands::Checkpoint {
                snapshot,
                legacy_id,
                ..
            } => {
                assert!(snapshot.is_none());
                assert!(legacy_id.is_none());
            }
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn generate_auto_id_matches_expected_format() {
        let id = generate_auto_id();
        assert!(
            id.starts_with("ckpt-"),
            "auto id should start with ckpt- prefix"
        );
        assert_eq!(
            id.len(),
            "ckpt-YYYYMMDDTHHmmss.fff".len(),
            "auto id {:?} has unexpected length",
            id
        );
        let body = &id[5..];
        assert!(
            body.chars()
                .all(|c| c.is_ascii_digit() || c == 'T' || c == '.'),
            "auto id body {:?} contains unexpected characters",
            body
        );
    }

    #[test]
    fn rollback_missing_both_target_and_ancestors_parses_but_fields_none() {
        let cli = Cli::try_parse_from(["ws-ckpt", "rollback", "--workspace", "/ws"]).unwrap();
        match cli.command {
            Commands::Rollback {
                to, num_ancestors, ..
            } => {
                assert!(to.is_none());
                assert!(num_ancestors.is_none());
            }
            _ => panic!("expected Rollback"),
        }
    }

    // ── Metadata JSON validation ──
    // Note: clap accepts --metadata as a raw string. JSON validation happens
    // at runtime in the run() function via serde_json::from_str, not at parse time.
    // So both valid and invalid JSON will parse successfully at the clap level.

    #[test]
    fn metadata_valid_json_parses() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "checkpoint",
            "--workspace",
            "/ws",
            "--snapshot",
            "snap-1",
            "--metadata",
            r#"{"key":"value"}"#,
        ])
        .unwrap();
        match cli.command {
            Commands::Checkpoint { metadata, .. } => {
                let json_str = metadata.unwrap();
                // Verify it's valid JSON
                let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
                assert_eq!(v["key"], "value");
            }
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn metadata_invalid_json_parsed_but_invalid() {
        // clap will accept the string, but serde_json::from_str should fail
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "checkpoint",
            "--workspace",
            "/ws",
            "--snapshot",
            "snap-1",
            "--metadata",
            "not-json",
        ])
        .unwrap();
        match cli.command {
            Commands::Checkpoint { metadata, .. } => {
                let json_str = metadata.unwrap();
                let result = serde_json::from_str::<serde_json::Value>(&json_str);
                assert!(result.is_err(), "invalid JSON should fail to parse");
            }
            _ => panic!("expected Checkpoint"),
        }
    }

    // ── handle_response: DeleteOk output branching ──

    #[tokio::test]
    async fn delete_ok_snapshot_shows_deleted_message() {
        let response = Response::DeleteOk {
            target: "msg1-step0".to_string(),
        };
        let request = Request::Delete {
            workspace: Some("/ws".to_string()),
            snapshot: "msg1-step0".to_string(),
            force: false,
        };
        let result = handle_response(response, &request).await;
        assert!(result.is_ok());
    }

    // ── Phase 2 CLI parsing tests ──

    #[test]
    fn parse_list() {
        let cli = Cli::try_parse_from(["ws-ckpt", "list", "--workspace", "/tmp/test"]).unwrap();
        match cli.command {
            Commands::List {
                workspace, format, ..
            } => {
                assert_eq!(workspace.as_deref(), Some("/tmp/test"));
                assert_eq!(format, "table"); // default
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn parse_list_orphans_with_and_without_workspace() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "list",
            "-w",
            "/ws",
            "--orphans",
            "--format",
            "json",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Commands::List { workspace: Some(ws), orphans: true, format, .. } if ws == "/ws" && format == "json")
        );
        let cli = Cli::try_parse_from(["ws-ckpt", "list", "--orphans"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::List {
                workspace: None,
                orphans: true,
                ..
            }
        ));
    }

    #[test]
    fn parse_list_pagination() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "list",
            "--limit",
            "10",
            "--cursor",
            "opaque-cursor",
            "--format",
            "json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::List {
                format,
                limit: Some(10),
                cursor: Some(cursor),
                ..
            } if format == "json" && cursor == "opaque-cursor"
        ));
        assert!(Cli::try_parse_from(["ws-ckpt", "list", "--limit", "0"]).is_err());
        assert!(Cli::try_parse_from(["ws-ckpt", "list", "--offset", "20"]).is_err());
        let cli = Cli::try_parse_from(["ws-ckpt", "list", "--cursor", "next"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::List {
                limit: None,
                cursor: Some(cursor),
                ..
            } if cursor == "next"
        ));
    }

    #[test]
    fn aggregate_list_combines_pages_into_one_json_array() {
        let _ = take_json_output();
        let first = SnapshotListItem::Summary(ws_ckpt_common::SnapshotSummary {
            id: "s1".into(),
            workspace: "/ws".into(),
            created_at: chrono::Utc::now(),
            pinned: false,
            missing: false,
            omitted_fields: vec!["message".into()],
        });
        let second = SnapshotListItem::Summary(ws_ckpt_common::SnapshotSummary {
            id: "s2".into(),
            workspace: "/ws".into(),
            created_at: chrono::Utc::now(),
            pinned: false,
            missing: false,
            omitted_fields: vec!["metadata".into()],
        });
        let mut all = Vec::new();
        assert_eq!(
            accept_list_page(
                &mut all,
                Response::ListPageOk {
                    snapshots: vec![first],
                    next_cursor: Some("next".into()),
                },
            )
            .unwrap()
            .as_deref(),
            Some("next")
        );
        assert_eq!(
            accept_list_page(
                &mut all,
                Response::ListPageOk {
                    snapshots: vec![second],
                    next_cursor: None,
                },
            )
            .unwrap(),
            None
        );
        render_snapshot_list(all, None, "json", false).unwrap();
        let output = take_json_output().expect("single JSON output");
        assert_eq!(output.as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn aggregate_list_failure_does_not_emit_partial_json() {
        let _ = take_json_output();
        let summary = SnapshotListItem::Summary(ws_ckpt_common::SnapshotSummary {
            id: "s1".into(),
            workspace: "/ws".into(),
            created_at: chrono::Utc::now(),
            pinned: false,
            missing: false,
            omitted_fields: vec!["message".into()],
        });
        let mut all = Vec::new();
        let cursor = accept_list_page(
            &mut all,
            Response::ListPageOk {
                snapshots: vec![summary],
                next_cursor: Some("next".into()),
            },
        )
        .unwrap();
        assert_eq!(cursor.as_deref(), Some("next"));
        let error = accept_list_page(
            &mut all,
            Response::Error {
                code: ErrorCode::InternalError,
                message: "later page failed".into(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("later page failed"));
        assert!(take_json_output().is_none());
    }

    #[test]
    fn summary_json_preserves_metadata_paths_and_detail_marker() {
        let _ = take_json_output();
        let created_at = chrono::Utc::now();
        render_snapshot_list(
            vec![
                SnapshotListItem::Full(ws_ckpt_common::SnapshotEntry {
                    id: "s-full".into(),
                    workspace: "/ws".into(),
                    meta: ws_ckpt_common::SnapshotMeta {
                        created_at,
                        pinned: true,
                        missing: false,
                        message: None,
                        metadata: None,
                        parent_id: None,
                        child_ids: vec![],
                    },
                }),
                SnapshotListItem::Summary(ws_ckpt_common::SnapshotSummary {
                    id: "s-large".into(),
                    workspace: "/ws".into(),
                    created_at,
                    pinned: true,
                    missing: false,
                    omitted_fields: vec![
                        "message".into(),
                        "metadata".into(),
                        "parent_id".into(),
                        "child_ids".into(),
                    ],
                }),
            ],
            None,
            "json",
            false,
        )
        .unwrap();
        let output = take_json_output().unwrap();
        assert_eq!(output[1]["detail"], "summary");
        for field in ["created_at", "pinned", "missing"] {
            assert_eq!(output[0]["meta"][field], output[1]["meta"][field]);
            assert!(output[1].get(field).is_none());
        }
        assert_eq!(
            output[1]["meta"]["created_at"],
            serde_json::to_value(created_at).unwrap()
        );
        assert_eq!(output[1]["meta"]["pinned"], true);
        assert_eq!(output[1]["meta"]["missing"], false);
        for field in ["message", "metadata", "parent_id", "child_ids"] {
            assert!(output[1]["meta"].get(field).is_none());
            assert!(output[1]["omitted_fields"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(field)));
        }
    }

    #[test]
    fn parse_list_rejects_invalid_format() {
        let error = Cli::try_parse_from(["ws-ckpt", "list", "--format", "jso"])
            .err()
            .expect("invalid list format must be rejected");
        assert!(error.to_string().contains("invalid value 'jso'"));
    }

    #[test]
    fn parse_list_json_format() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "list",
            "--workspace",
            "/tmp/test",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Commands::List { format, .. } => {
                assert_eq!(format, "json");
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn parse_diff() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "diff",
            "--workspace",
            "/tmp/test",
            "--from",
            "msg1-step0",
            "--to",
            "msg2-step0",
        ])
        .unwrap();
        match cli.command {
            Commands::Diff {
                workspace,
                from,
                to,
            } => {
                assert_eq!(workspace, "/tmp/test");
                assert_eq!(from, "msg1-step0");
                assert_eq!(to, Some("msg2-step0".to_string()));
            }
            _ => panic!("expected Diff"),
        }
    }

    #[test]
    fn parse_status() {
        let cli = Cli::try_parse_from(["ws-ckpt", "status"]).unwrap();
        match cli.command {
            Commands::Status { workspace, format } => {
                assert!(workspace.is_none());
                assert_eq!(format, "table");
            }
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn parse_status_with_workspace() {
        let cli = Cli::try_parse_from(["ws-ckpt", "status", "--workspace", "/tmp/ws"]).unwrap();
        match cli.command {
            Commands::Status { workspace, format } => {
                assert_eq!(workspace.as_deref(), Some("/tmp/ws"));
                assert_eq!(format, "table");
            }
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn parse_status_rejects_invalid_format() {
        let error = Cli::try_parse_from(["ws-ckpt", "status", "--format", "jso"])
            .err()
            .expect("invalid status format must be rejected");
        assert!(error.to_string().contains("invalid value 'jso'"));
    }

    #[test]
    fn parse_status_json_format() {
        let cli = Cli::try_parse_from(["ws-ckpt", "status", "--format", "json"]).unwrap();
        match cli.command {
            Commands::Status { format, .. } => {
                assert_eq!(format, "json");
            }
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn parse_cleanup() {
        let cli = Cli::try_parse_from(["ws-ckpt", "cleanup", "--workspace", "/tmp/test"]).unwrap();
        match cli.command {
            Commands::Cleanup { workspace, keep } => {
                assert_eq!(workspace, "/tmp/test");
                assert_eq!(keep, 20); // default
            }
            _ => panic!("expected Cleanup"),
        }
    }

    #[test]
    fn parse_cleanup_with_keep() {
        let cli = Cli::try_parse_from([
            "ws-ckpt",
            "cleanup",
            "--workspace",
            "/tmp/test",
            "--keep",
            "5",
        ])
        .unwrap();
        match cli.command {
            Commands::Cleanup { keep, .. } => {
                assert_eq!(keep, 5);
            }
            _ => panic!("expected Cleanup"),
        }
    }

    #[test]
    fn list_without_workspace_parses_ok() {
        let cli = Cli::try_parse_from(["ws-ckpt", "list"]).unwrap();
        match cli.command {
            Commands::List {
                workspace, format, ..
            } => {
                assert!(workspace.is_none());
                assert_eq!(format, "table");
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn diff_missing_from_fails() {
        let result = Cli::try_parse_from([
            "ws-ckpt",
            "diff",
            "--workspace",
            "/ws",
            "--to",
            "msg1-step0",
        ]);
        assert!(result.is_err(), "diff without --from should fail");
    }

    #[test]
    fn cleanup_missing_workspace_fails() {
        let result = Cli::try_parse_from(["ws-ckpt", "cleanup"]);
        assert!(result.is_err(), "cleanup without --workspace should fail");
    }

    fn parse_config_args(argv: &[&str]) -> ConfigArgs {
        let cli = Cli::try_parse_from(argv).expect("config args should parse");
        match cli.command {
            Commands::Config(a) => a,
            _ => panic!("expected Config"),
        }
    }

    #[test]
    fn parse_config_no_scope_is_overview() {
        // `ws-ckpt config` (no -g/-w, no flags) parses successfully — it's the
        // view-only overview. Update flags without scope are rejected at
        // runtime, not by clap.
        let args = parse_config_args(&["ws-ckpt", "config"]);
        assert!(!args.global);
        assert!(args.workspace.is_none());
    }

    #[test]
    fn parse_config_global_and_workspace_conflict() {
        let result = Cli::try_parse_from(["ws-ckpt", "config", "-g", "-w", "/tmp/ws"]);
        assert!(
            result.is_err(),
            "-g and -w should be mutually exclusive (ArgGroup)"
        );
    }

    #[test]
    fn parse_config_global_view() {
        let args = parse_config_args(&["ws-ckpt", "config", "-g"]);
        assert!(args.global);
        assert!(args.workspace.is_none());
        assert!(args.health_check_interval.is_none());
    }

    #[test]
    fn parse_config_global_with_all_flags() {
        let args = parse_config_args(&[
            "ws-ckpt",
            "config",
            "-g",
            "--health-check-interval",
            "120",
            "--img-size",
            "30",
            "--img-max-percent",
            "40",
        ]);
        assert!(args.global);
        assert_eq!(args.health_check_interval, Some(120));
        assert_eq!(args.img_size, Some(30));
        assert_eq!(args.img_max_percent, Some(40.0));
    }

    #[test]
    fn parse_reload() {
        let cli = Cli::try_parse_from(["ws-ckpt", "reload"]).unwrap();
        assert!(matches!(cli.command, Commands::Reload));
    }

    #[test]
    fn parse_reload_rejects_args() {
        let result = Cli::try_parse_from(["ws-ckpt", "reload", "--foo"]);
        assert!(result.is_err(), "reload should accept no arguments");
    }

    #[test]
    fn parse_config_global_enable_auto_cleanup() {
        let args = parse_config_args(&["ws-ckpt", "config", "-g", "--enable-auto-cleanup"]);
        assert!(args.enable_auto_cleanup);
        assert!(!args.disable_auto_cleanup);
    }

    #[test]
    fn parse_config_global_disable_auto_cleanup() {
        let args = parse_config_args(&["ws-ckpt", "config", "-g", "--disable-auto-cleanup"]);
        assert!(!args.enable_auto_cleanup);
        assert!(args.disable_auto_cleanup);
    }

    #[test]
    fn parse_config_enable_disable_conflict() {
        let result = Cli::try_parse_from([
            "ws-ckpt",
            "config",
            "-g",
            "--enable-auto-cleanup",
            "--disable-auto-cleanup",
        ]);
        assert!(
            result.is_err(),
            "--enable-auto-cleanup and --disable-auto-cleanup must be mutually exclusive"
        );
    }

    #[test]
    fn parse_config_auto_cleanup_keep_count() {
        let args = parse_config_args(&["ws-ckpt", "config", "-g", "--auto-cleanup-keep", "20"]);
        assert_eq!(args.auto_cleanup_keep, Some(CleanupRetention::Count(20)));
    }

    #[test]
    fn parse_config_auto_cleanup_keep_zero_disables() {
        let args = parse_config_args(&["ws-ckpt", "config", "-g", "--auto-cleanup-keep", "0"]);
        let keep = args.auto_cleanup_keep.expect("keep should be set");
        assert!(keep.is_disabled());
        assert_eq!(keep, CleanupRetention::Count(0));
    }

    #[test]
    fn parse_config_auto_cleanup_keep_age() {
        let args = parse_config_args(&["ws-ckpt", "config", "-g", "--auto-cleanup-keep", "30d"]);
        match args.auto_cleanup_keep {
            Some(CleanupRetention::Age { raw, secs }) => {
                assert_eq!(raw, "30d");
                assert_eq!(secs, 30 * 24 * 3600);
            }
            other => panic!("expected Age variant, got {:?}", other),
        }
    }

    #[test]
    fn parse_config_auto_cleanup_keep_invalid() {
        let result = Cli::try_parse_from(["ws-ckpt", "config", "-g", "--auto-cleanup-keep", "abc"]);
        assert!(
            result.is_err(),
            "non-numeric value without duration unit should be rejected"
        );
    }

    #[test]
    fn parse_config_auto_cleanup_interval() {
        let args =
            parse_config_args(&["ws-ckpt", "config", "-g", "--auto-cleanup-interval", "3600"]);
        assert_eq!(args.auto_cleanup_interval, Some(3600));
    }

    #[test]
    fn parse_config_workspace_view() {
        let args = parse_config_args(&["ws-ckpt", "config", "-w", "/tmp/proj"]);
        assert!(!args.global);
        assert_eq!(args.workspace.as_deref(), Some("/tmp/proj"));
        assert!(!args.reset);
    }

    #[test]
    fn parse_config_workspace_reset() {
        let args = parse_config_args(&["ws-ckpt", "config", "-w", "/tmp/proj", "--reset"]);
        assert!(args.reset);
        assert_eq!(args.workspace.as_deref(), Some("/tmp/proj"));
    }

    #[test]
    fn parse_config_workspace_set_keep() {
        let args = parse_config_args(&[
            "ws-ckpt",
            "config",
            "-w",
            "/tmp/proj",
            "--auto-cleanup-keep",
            "5",
        ]);
        assert_eq!(args.auto_cleanup_keep, Some(CleanupRetention::Count(5)));
    }

    #[test]
    fn parse_config_reset_conflicts_with_other_updates() {
        let result = Cli::try_parse_from([
            "ws-ckpt",
            "config",
            "-w",
            "/tmp/proj",
            "--reset",
            "--auto-cleanup-keep",
            "5",
        ]);
        assert!(
            result.is_err(),
            "--reset must conflict with other update flags"
        );
    }

    #[test]
    fn parse_config_format_defaults_to_text() {
        let args = parse_config_args(&["ws-ckpt", "config", "-g"]);
        assert_eq!(args.format, "text");
        assert_eq!(
            parse_output_format(&args.format).unwrap(),
            OutputFormat::Text
        );
    }

    #[test]
    fn parse_config_format_json_accepted() {
        let args = parse_config_args(&["ws-ckpt", "config", "-g", "--format", "json"]);
        assert_eq!(args.format, "json");
        assert_eq!(
            parse_output_format(&args.format).unwrap(),
            OutputFormat::Json
        );
    }

    #[test]
    fn parse_output_format_rejects_typo() {
        // Anything other than text/json must fail loudly — a silent
        // fallback to text would break plugin scripts that asked for JSON.
        assert!(parse_output_format("jso").is_err());
        assert!(parse_output_format("YAML").is_err());
        assert!(parse_output_format("").is_err());
    }

    #[test]
    fn workspace_policy_json_schema_is_versioned() {
        // The `schema` field lets consumers reject an unknown version. This
        // pins v1: a deliberate shape change must also bump this assert, so
        // an accidental field rename is caught here.
        use ws_ckpt_common::{EffectivePolicy, GlobalPolicySnapshot, WorkspacePolicy};
        let effective = EffectivePolicy {
            auto_cleanup: true,
            auto_cleanup_keep: CleanupRetention::Count(5),
        };
        let local = WorkspacePolicy::default();
        let global = GlobalPolicySnapshot {
            auto_cleanup: true,
            auto_cleanup_keep: CleanupRetention::Count(20),
        };
        let json =
            WorkspacePolicyJson::from_views("ws-test".to_string(), &effective, &local, &global);
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.contains(r#""schema":"ws-ckpt-policy/v1""#));
        // Tagged retention (consumer can match on `mode`, no number-vs-
        // string discrimination needed).
        assert!(s.contains(r#""mode":"count""#));
        assert!(s.contains(r#""count":5"#));
        // is_disabled pre-computed on the wire.
        assert!(s.contains(r#""is_disabled":false"#));
    }

    #[test]
    fn workspace_policy_json_age_retention_emits_tagged_form() {
        use ws_ckpt_common::{EffectivePolicy, GlobalPolicySnapshot, WorkspacePolicy};
        let effective = EffectivePolicy {
            auto_cleanup: true,
            auto_cleanup_keep: CleanupRetention::age("30d").unwrap(),
        };
        let local = WorkspacePolicy::default();
        let global = GlobalPolicySnapshot {
            auto_cleanup: true,
            auto_cleanup_keep: CleanupRetention::Count(20),
        };
        let json =
            WorkspacePolicyJson::from_views("ws-test".to_string(), &effective, &local, &global);
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.contains(r#""mode":"age""#));
        assert!(s.contains(r#""raw":"30d""#));
        assert!(s.contains(r#""secs":2592000"#));
    }

    #[test]
    fn workspace_policy_json_count_zero_is_disabled() {
        // Regression: openclaw rendered `Count(0)` as "0" with no disabled
        // marker, though the scheduler skips it. Pre-computing is_disabled
        // via from_views(... &effective ...) walks the production path,
        // so a future regression that breaks `is_disabled()` would fail
        // here too — not just the wire format.
        use ws_ckpt_common::{EffectivePolicy, GlobalPolicySnapshot, WorkspacePolicy};
        let effective = EffectivePolicy {
            auto_cleanup: true,
            auto_cleanup_keep: CleanupRetention::Count(0),
        };
        let local = WorkspacePolicy::default();
        let global = GlobalPolicySnapshot {
            auto_cleanup: true,
            auto_cleanup_keep: CleanupRetention::Count(0),
        };
        let json =
            WorkspacePolicyJson::from_views("ws-test".to_string(), &effective, &local, &global);
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.contains(r#""mode":"count""#));
        assert!(s.contains(r#""count":0"#));
        // Critical: is_disabled MUST be true even though auto_cleanup=true,
        // because keep is Count(0). That's the whole bug this prevents.
        assert!(s.contains(r#""is_disabled":true"#));
    }

    #[test]
    fn parse_unregister_requires_workspace_and_keeps_force_optional() {
        assert!(Cli::try_parse_from(["ws-ckpt", "unregister"]).is_err());
        let cli =
            Cli::try_parse_from(["ws-ckpt", "unregister", "-w", "/tmp/ws", "--force"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Unregister { workspace, force: true } if workspace == "/tmp/ws")
        );
    }

    #[test]
    fn recovery_confirmation_uses_daemon_identity_and_refuses_missing_preview() {
        let preview = RecoveryPreview {
            ws_id: Some("ws-actual".into()),
            registration_path: "/real-parent/repo".into(),
            snapshot_count: 19,
            confirmation_digest: [7; 32],
        };
        let received = recovery_preview_from_response(Response::RecoverPreviewOk {
            preview: preview.clone(),
        })
        .unwrap();
        assert_eq!(received, preview);
        assert!(recovery_preview_from_response(Response::RecoverOk {
            workspace: "/alias-parent/repo".into(),
        })
        .unwrap_err()
        .to_string()
        .contains("upgrade/restart"));
        assert!(recovery_preview_from_response(Response::Error {
            code: ErrorCode::WorkspaceNotFound,
            message: "registration disappeared".into(),
        })
        .unwrap_err()
        .to_string()
        .contains("registration disappeared"));
    }

    // ── Recover CLI parsing tests ──

    #[test]
    fn parse_recover_with_workspace() {
        let cli =
            Cli::try_parse_from(["ws-ckpt", "recover", "--workspace", "/tmp/my-project"]).unwrap();
        match cli.command {
            Commands::Recover {
                workspace,
                all,
                force,
            } => {
                assert_eq!(workspace.as_deref(), Some("/tmp/my-project"));
                assert!(!all);
                assert!(!force);
            }
            _ => panic!("expected Recover"),
        }
    }

    #[test]
    fn parse_recover_with_short_workspace() {
        let cli = Cli::try_parse_from(["ws-ckpt", "recover", "-w", "/tmp/ws"]).unwrap();
        match cli.command {
            Commands::Recover { workspace, .. } => {
                assert_eq!(workspace.as_deref(), Some("/tmp/ws"));
            }
            _ => panic!("expected Recover"),
        }
    }

    #[test]
    fn parse_recover_all() {
        let cli = Cli::try_parse_from(["ws-ckpt", "recover", "--all"]).unwrap();
        match cli.command {
            Commands::Recover {
                workspace,
                all,
                force,
            } => {
                assert!(workspace.is_none());
                assert!(all);
                assert!(!force);
            }
            _ => panic!("expected Recover"),
        }
    }

    #[test]
    fn parse_recover_all_force() {
        let cli = Cli::try_parse_from(["ws-ckpt", "recover", "--all", "--force"]).unwrap();
        match cli.command {
            Commands::Recover {
                workspace,
                all,
                force,
            } => {
                assert!(workspace.is_none());
                assert!(all);
                assert!(force);
            }
            _ => panic!("expected Recover"),
        }
    }

    #[test]
    fn parse_recover_no_args_parses_ok() {
        // No args is valid at parse time; runtime validates
        let cli = Cli::try_parse_from(["ws-ckpt", "recover"]).unwrap();
        match cli.command {
            Commands::Recover {
                workspace,
                all,
                force,
            } => {
                assert!(workspace.is_none());
                assert!(!all);
                assert!(!force);
            }
            _ => panic!("expected Recover"),
        }
    }

    #[test]
    fn parse_recover_workspace_and_all_conflicts() {
        let result = Cli::try_parse_from(["ws-ckpt", "recover", "--workspace", "/ws", "--all"]);
        assert!(result.is_err(), "--workspace and --all should conflict");
    }

    #[test]
    fn parse_recover_workspace_force() {
        let cli = Cli::try_parse_from(["ws-ckpt", "recover", "-w", "/tmp/ws", "--force"]).unwrap();
        match cli.command {
            Commands::Recover {
                workspace,
                all,
                force,
            } => {
                assert_eq!(workspace.as_deref(), Some("/tmp/ws"));
                assert!(!all);
                assert!(force);
            }
            _ => panic!("expected Recover"),
        }
    }

    // ── unlanded_global_settings: `config -g` write must actually reach the daemon ──

    fn img_config(
        img_size: Option<u64>,
        img_max_percent: Option<f64>,
    ) -> ws_ckpt_common::FileConfig {
        ws_ckpt_common::FileConfig {
            backend: ws_ckpt_common::BackendConfig {
                btrfs_loop: Some(ws_ckpt_common::BtrfsLoopConfig {
                    img_size,
                    img_max_percent,
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn no_unlanded_settings_when_daemon_reloaded_same_file() {
        let saved = ws_ckpt_common::FileConfig {
            auto_cleanup: Some(true),
            auto_cleanup_keep: Some(CleanupRetention::Count(3)),
            health_check_interval_secs: Some(30),
            ..Default::default()
        };
        assert!(unlanded_global_settings(&saved, &saved).is_empty());
    }

    #[test]
    fn unlanded_settings_reported_when_daemon_never_saw_the_file() {
        // Sidecar case: daemon's own /etc/ws-ckpt/config.toml is absent, so it
        // reloads into built-in defaults while the CLI wrote a real policy.
        let saved = ws_ckpt_common::FileConfig {
            auto_cleanup: Some(true),
            auto_cleanup_keep: Some(CleanupRetention::Count(3)),
            auto_cleanup_interval_secs: Some(600),
            health_check_interval_secs: Some(30),
            ..Default::default()
        };
        let unlanded = unlanded_global_settings(&saved, &ws_ckpt_common::FileConfig::default());
        assert_eq!(unlanded.len(), 4, "got: {:?}", unlanded);
        assert!(unlanded[0].starts_with("auto-cleanup: file has true, daemon has unset"));
    }

    #[test]
    fn img_only_write_is_reported_when_daemon_never_saw_the_file() {
        // Bootstrap-only settings are the case a runtime view cannot catch: the
        // daemon would keep the running image size either way.
        let saved = img_config(Some(64), Some(75.0));
        let unlanded = unlanded_global_settings(&saved, &ws_ckpt_common::FileConfig::default());
        assert_eq!(unlanded.len(), 2, "got: {:?}", unlanded);
        assert!(unlanded[0].starts_with("img-size: file has 64, daemon has unset"));
    }

    #[test]
    fn img_write_the_daemon_read_is_not_reported() {
        let saved = img_config(Some(64), None);
        assert!(unlanded_global_settings(&saved, &saved).is_empty());
    }

    #[test]
    fn age_retention_compares_by_value() {
        let saved = ws_ckpt_common::FileConfig {
            auto_cleanup_keep: Some(CleanupRetention::age("30d").unwrap()),
            ..Default::default()
        };
        assert!(unlanded_global_settings(&saved, &saved).is_empty());

        let stale = ws_ckpt_common::FileConfig {
            auto_cleanup_keep: Some(CleanupRetention::age("7d").unwrap()),
            ..Default::default()
        };
        assert_eq!(unlanded_global_settings(&saved, &stale).len(), 1);
    }

    // ── disabled_warning_for: warn whenever the user touched a policy field but effective stays disabled ──

    fn eff(cleanup: bool, keep: CleanupRetention) -> ws_ckpt_common::EffectivePolicy {
        ws_ckpt_common::EffectivePolicy {
            auto_cleanup: cleanup,
            auto_cleanup_keep: keep,
        }
    }

    #[test]
    fn warn_enable_only_when_global_keep_is_zero() {
        // Old gate: --enable-auto-cleanup, no --keep, global keep is 0.
        let e = eff(true, CleanupRetention::Count(0));
        let l = ws_ckpt_common::WorkspacePolicy::default();
        let msg = disabled_warning_for(true, false, &e, &l).expect("warning expected");
        assert!(
            msg.contains("global auto_cleanup_keep is 0"),
            "got: {}",
            msg
        );
    }

    #[test]
    fn warn_enable_with_keep_zero_supplied() {
        // #5: --enable-auto-cleanup --auto-cleanup-keep 0 must still warn.
        let e = eff(true, CleanupRetention::Count(0));
        let l = ws_ckpt_common::WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: Some(CleanupRetention::Count(0)),
        };
        let msg = disabled_warning_for(true, true, &e, &l).expect("warning expected");
        assert!(msg.contains("local auto_cleanup_keep is 0"), "got: {}", msg);
    }

    #[test]
    fn warn_keep_only_when_global_cleanup_is_off() {
        // #6: --auto-cleanup-keep 5 alone, global cleanup is false.
        let e = eff(false, CleanupRetention::Count(5));
        let l = ws_ckpt_common::WorkspacePolicy {
            auto_cleanup: None,
            auto_cleanup_keep: Some(CleanupRetention::Count(5)),
        };
        let msg = disabled_warning_for(false, true, &e, &l).expect("warning expected");
        assert!(msg.contains("global auto_cleanup is false"), "got: {}", msg);
    }

    #[test]
    fn no_warn_when_effective_active() {
        let e = eff(true, CleanupRetention::Count(20));
        let l = ws_ckpt_common::WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: Some(CleanupRetention::Count(20)),
        };
        assert!(disabled_warning_for(true, true, &e, &l).is_none());
    }

    #[test]
    fn no_warn_when_user_supplied_nothing() {
        // Daemon view alone (no PATCH fields) must not produce a warning.
        let e = eff(false, CleanupRetention::Count(0));
        let l = ws_ckpt_common::WorkspacePolicy::default();
        assert!(disabled_warning_for(false, false, &e, &l).is_none());
    }
}
