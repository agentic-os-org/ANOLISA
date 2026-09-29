use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use agent_memory::config::AppConfig;
use agent_memory::mcp_server::MemoryMcpServer;
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;

#[derive(Parser)]
#[command(name = "agent-memory")]
#[command(about = "Agent memory — filesystem memory MCP server")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file
    #[arg(long, global = true)]
    config: Option<String>,

    /// Mount strategy override: auto | userland | userns
    #[arg(long, global = true)]
    mount_strategy: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start as MCP server (default when no subcommand)
    Serve,
    /// Initialize the namespace mount (creates `<base>/<ns>/.anolisa/` and README.md).
    Init,
    /// Print resolved configuration: mount path, profile, ns.
    Info,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Before anything below can change the answer: `early_enter_userns`
    // maps our uid to 0, and the per-user session fallback has to be named
    // after the *host* uid or every user on the box collides on
    // `/tmp/anolisa-sessions-0`.
    agent_memory::host::capture_host_uid();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let config_path = cli.config.as_deref().map(std::path::Path::new);
    let mut config = AppConfig::load(config_path)?;

    // CLI flag wins over config + env.
    if let Some(s) = &cli.mount_strategy {
        match MountStrategyKind::from_str_loose(s) {
            Some(k) => config.memory.mount.strategy = k,
            None => {
                anyhow::bail!("invalid --mount-strategy '{s}'; expected auto | userland | userns")
            }
        }
    }

    // P6.4: cgroup memory.max has to land before the runtime starts so
    // tokio workers land in the limited cgroup too. We also apply it
    // BEFORE unshare(NEWUSER): writes to /sys/fs/cgroup are evaluated
    // against the caller's real uid, and some kernels reject sysfs writes
    // from inside an unprivileged user namespace (the uid_map mapping
    // inside-0 → outside-real does not always extend to cgroup writes).
    // Order: cgroup → unshare.
    early_apply_cgroup(&config);

    // CRITICAL: unshare(CLONE_NEWUSER) requires the calling thread to be
    // the only thread in the process. Tokio's default multi-thread runtime
    // spawns workers BEFORE we reach `MemoryService::new`, which would make
    // any subsequent unshare fail with EINVAL. We therefore enter the user
    // namespace synchronously here, before constructing any runtime.
    early_enter_userns(&config);

    // Now safe to build the multi-threaded tokio runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        match cli.command {
            None | Some(Commands::Serve) => run_mcp_server(config).await,
            Some(Commands::Init) => cmd_init(config).await,
            Some(Commands::Info) => cmd_info(config).await,
        }
    })
}

/// Best-effort: if the configured strategy might want a user namespace,
/// try to enter one while the process is still single-threaded. Failure
/// is logged at debug level — `MemoryService::new` will retry (auto:
/// fallback to userland; userns: fail loudly).
fn early_enter_userns(config: &AppConfig) {
    use agent_memory::mount::linux_userns::LinuxUserNsMount;
    match config.memory.mount.strategy {
        MountStrategyKind::Auto | MountStrategyKind::Userns => {
            if let Err(e) = LinuxUserNsMount::enter() {
                tracing::debug!("early unshare failed: {e}");
            }
        }
        MountStrategyKind::Userland => {}
    }
}

/// Best-effort: apply the cgroup v2 memory limit before tokio spawns its
/// worker threads, so child threads land in the limited cgroup too.
fn early_apply_cgroup(config: &AppConfig) {
    use agent_memory::cgroup::{CgroupOutcome, apply};
    match apply(&config.memory.cgroup) {
        CgroupOutcome::Joined { path, memory_max } => {
            tracing::info!(
                "joined cgroup {} with memory.max={}",
                path.display(),
                memory_max
            );
        }
        CgroupOutcome::Skipped => {}
        CgroupOutcome::Failed(e) => {
            tracing::warn!("cgroup quota not applied: {e}");
        }
    }
}

async fn run_mcp_server(config: AppConfig) -> Result<()> {
    tracing::info!("Starting agent-memory MCP server");
    tracing::info!("user_id: {}", config.global.user_id);
    tracing::info!("profile: {:?}", config.memory.profile);

    let svc = Arc::new(MemoryService::new(config)?);
    tracing::info!("mount: {}", svc.mount.root.display());
    if let Some(s) = &svc.session {
        tracing::info!("session: {} ({})", s.sid(), s.display_root().display());
    }

    let server = MemoryMcpServer::new(Arc::clone(&svc));

    let service = rmcp::serve_server(server, rmcp::transport::io::stdio())
        .await
        .map_err(|e: std::io::Error| anyhow::anyhow!("MCP server error: {e}"))?;

    // systemd sends SIGTERM by default when stopping a unit; without a
    // handler we'd be SIGKILL'd after TimeoutStopSec and skip
    // try_end_session, leaving session scratch behind.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| anyhow::anyhow!("install SIGTERM handler: {e}"))?;

    tokio::select! {
        r = service.waiting() => {
            if let Err(e) = r {
                tracing::warn!("MCP service ended with error: {e}");
            }
        },
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl_c received, shutting down");
        },
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM received, shutting down");
        },
    }

    // Run automatic memory consolidation before discarding the session.
    // Consolidation must happen BEFORE try_end_session because it reads
    // the session log which gets destroyed when the session is discarded.
    tracing::info!("running memory consolidation...");
    svc.consolidate();

    // Best-effort cleanup: remove the session scratch dir if config says so.
    let action = svc.config.memory.session.end_action;
    svc.try_end_session(action);
    tracing::info!("shutdown complete");

    Ok(())
}

async fn cmd_init(config: AppConfig) -> Result<()> {
    let svc = MemoryService::new(config)?;
    println!("Initialized memory mount: {}", svc.mount.root.display());
    println!("Audit log: {}", svc.mount.audit_log_path().display());
    println!("Profile: {:?}", svc.config.memory.profile);
    Ok(())
}

async fn cmd_info(config: AppConfig) -> Result<()> {
    let svc = MemoryService::new(config)?;
    println!("user_id        : {}", svc.config.global.user_id);
    println!("profile        : {:?}", svc.config.memory.profile);
    println!(
        "mount strategy : {} (configured: {})",
        svc.mount_strategy_name,
        svc.config.memory.mount.strategy.as_str()
    );
    println!("entered userns : {}", svc.entered_userns);
    println!(
        "base_dir       : {}",
        svc.config.resolved_base_dir().display()
    );
    println!("ns             : {}", svc.mount.ns.dir_name());
    println!("mount root     : {}", svc.mount.root.display());
    println!("meta dir       : {}", svc.mount.meta_dir.display());
    println!("audit log      : {}", svc.mount.audit_log_path().display());
    Ok(())
}
